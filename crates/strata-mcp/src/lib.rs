//! `strata mcp`: an MCP server over stdio for one agent, answering from the daemon's shared
//! checker. The calling agent is the one whose worktree holds an MCP root, else the working
//! directory, else `--agent` / `STRATA_AGENT`. With no match every tool returns an error: it
//! never answers from the base.

pub mod format;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use strata_core::{Error, Result, client};
use tokio::sync::OnceCell;

#[derive(Debug, Clone)]
struct Agent {
    repo: PathBuf,
    name: String,
    path: PathBuf,
}

#[derive(Clone)]
pub struct Server {
    flag: Option<String>,
    agent: Arc<OnceCell<Agent>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DiagnosticsArgs {
    /// File relative to your worktree. Omit to check the whole project (at most 200 problems are listed).
    pub file: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct FileArgs {
    /// File relative to your worktree.
    pub file: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DevArgs {
    /// Command to run instead of the project's dev command.
    pub command: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RunArgs {
    /// Shell command, run in your worktree.
    pub command: String,
    /// Stop it after this many seconds (default 120).
    pub timeout_s: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ProcessArgs {
    /// Process id from strata_start_dev_server, strata_run or strata_status (not the OS pid).
    pub process_id: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct LogsArgs {
    /// Process id from strata_start_dev_server, strata_run or strata_status (not the OS pid).
    pub process_id: i64,
    /// How many lines from the end (default 200).
    pub lines: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PositionArgs {
    /// File relative to your worktree.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in UTF-16 code units.
    pub column: u32,
}

fn text(s: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(s)])
}

fn failure(s: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(s)])
}

/// PLAN: an agent over its memory budget is warned "through status, events and MCP", so every
/// answer carries the warning while it lasts.
async fn budget_note(agent: &Agent) -> String {
    let Ok(status) = blocking(|| client::get("/v1/status")).await else {
        return String::new();
    };
    let me = status["repos"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["agents"].as_array().cloned().unwrap_or_default())
        .find(|a| a["path"] == agent.path.to_str().unwrap_or_default());
    let Some(me) = me else { return String::new() };
    let mb = me["memory_bytes"].as_u64().unwrap_or(0) / 1048576;
    match me["budget"].as_str() {
        Some("over") => {
            format!("\nwarning: your processes use {mb} MB, over this agent's memory budget")
        }
        Some("warning") => {
            format!("\nwarning: your processes use {mb} MB, near this agent's memory budget")
        }
        _ => String::new(),
    }
}

/// `file:///a%20b` -> `/a b`.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = rest
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

fn agents(status: &Value) -> Vec<Agent> {
    let mut out = vec![];
    for repo in status["repos"].as_array().into_iter().flatten() {
        for a in repo["agents"].as_array().into_iter().flatten() {
            if a["state"] == "ready" {
                out.push(Agent {
                    repo: PathBuf::from(repo["root"].as_str().unwrap_or_default()),
                    name: a["name"].as_str().unwrap_or_default().into(),
                    path: PathBuf::from(a["path"].as_str().unwrap_or_default()),
                });
            }
        }
    }
    out
}

/// The agent whose worktree contains `path` (the deepest, for worktrees nested in others).
fn containing(agents: &[Agent], path: &Path) -> Option<Agent> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    agents
        .iter()
        .filter(|a| path.starts_with(&a.path))
        .max_by_key(|a| a.path.as_os_str().len())
        .cloned()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
}

#[tool_router]
impl Server {
    pub fn new(flag: Option<String>) -> Self {
        Self {
            flag,
            agent: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "TypeScript errors and warnings for one file, or the whole project when `file` is omitted, from Strata's shared type checker. Paths are relative to your worktree; lines and columns are 1-based."
    )]
    async fn strata_diagnostics(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<DiagnosticsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(
                &ctx,
                json!({ "op": "diagnostics", "file": a.file, "limit": 200 }),
                format::diagnostics,
            )
            .await)
    }

    #[tool(
        description = "Checks your changes against every other agent's, before either is merged: type errors that appear only once both are combined, and files you both changed that git cannot merge. strata_diagnostics also reports clashes found in the background."
    )]
    async fn strata_conflicts(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(&ctx, json!({ "op": "conflicts" }), format::conflicts)
            .await)
    }

    #[tool(description = "The type (and documentation) of the symbol at a position.")]
    async fn strata_hover(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<PositionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(
                &ctx,
                json!({ "op": "hover", "file": a.file, "line": a.line, "column": a.column }),
                format::hover,
            )
            .await)
    }

    #[tool(description = "Where the symbol at a position is declared, as file:line:column.")]
    async fn strata_definition(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<PositionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(
                &ctx,
                json!({ "op": "definition", "file": a.file, "line": a.line, "column": a.column }),
                format::locations,
            )
            .await)
    }

    #[tool(description = "Every reference to the symbol at a position, as file:line:column.")]
    async fn strata_references(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<PositionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(
                &ctx,
                json!({ "op": "references", "file": a.file, "line": a.line, "column": a.column }),
                format::locations,
            )
            .await)
    }

    #[tool(
        description = "Outline of a file's declarations (classes, functions, interfaces, members) with positions."
    )]
    async fn strata_symbols(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<FileArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .check(
                &ctx,
                json!({ "op": "symbols", "file": a.file }),
                format::symbols,
            )
            .await)
    }

    #[tool(
        description = "Start this worktree's dev server (the project's dev command unless `command` is given) and return its URL at once. Each agent has its own port and URL; if the server is already running, returns it."
    )]
    async fn strata_start_dev_server(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<DevArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .procs(&ctx, json!({ "kind": "dev", "cmd": a.command }), |v| {
                let state = if v["already_running"] == true { "already running" } else { "started" };
                format!(
                    "Dev server {state}: {}\nDirect URL (for OAuth callbacks that reject .localhost): {}\nProcess {} (`{}`); see its output with strata_logs.",
                    v["url"].as_str().unwrap_or("?"),
                    v["direct_url"].as_str().unwrap_or("?"),
                    v["process"]["id"],
                    v["process"]["cmd"].as_str().unwrap_or("?")
                )
            })
            .await)
    }

    #[tool(
        description = "Run a shell command in your worktree under Strata's supervision (it counts toward your memory budget and is cleaned up with the worktree). Returns the exit code and the last 200 lines of output."
    )]
    async fn strata_run(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<RunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let timeout = a.timeout_s.unwrap_or(120);
        Ok(self
            .procs(
                &ctx,
                json!({ "kind": "run", "cmd": a.command, "timeout_s": timeout }),
                move |v| {
                    let head = if v["timed_out"] == true {
                        format!("Stopped after {timeout} s (timeout).")
                    } else {
                        format!("Exit code {}.", v["exit_code"])
                    };
                    let lines: Vec<&str> = v["output"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str())
                        .collect();
                    format!("{head}\n{}", lines.join("\n"))
                },
            )
            .await)
    }

    #[tool(description = "Stop one of your processes (and everything it started).")]
    async fn strata_stop(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<ProcessArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = match self.resolve(&ctx).await {
            Ok(a) => a,
            Err(e) => return Ok(failure(e.to_string())),
        };
        let path = format!("/v1/procs/{}", a.process_id);
        let body = json!({ "agent": agent.name });
        Ok(
            match blocking(move || client::request("DELETE", &path, Some(body))).await {
                Ok(_) => text(format!("Stopped process {}.", a.process_id)),
                Err(e) => failure(e.to_string()),
            },
        )
    }

    #[tool(description = "The last lines of one of your processes' output.")]
    async fn strata_logs(
        &self,
        ctx: RequestContext<RoleServer>,
        Parameters(a): Parameters<LogsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = match self.resolve(&ctx).await {
            Ok(a) => a,
            Err(e) => return Ok(failure(e.to_string())),
        };
        let path = format!(
            "/v1/procs/{}/logs?lines={}&agent={}",
            a.process_id,
            a.lines.unwrap_or(200),
            agent.name
        );
        Ok(match blocking(move || client::get(&path)).await {
            Ok(v) => {
                let lines: Vec<&str> = v["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str())
                    .collect();
                text(if lines.is_empty() {
                    "No output yet.".into()
                } else {
                    lines.join("\n")
                })
            }
            Err(e) => failure(e.to_string()),
        })
    }

    #[tool(description = "This agent's worktree, branch and type-checker state.")]
    async fn strata_status(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let agent = match self.resolve(&ctx).await {
            Ok(a) => a,
            Err(e) => return Ok(failure(e.to_string())),
        };
        let status = match blocking(|| client::get("/v1/status")).await {
            Ok(s) => s,
            Err(e) => return Ok(failure(e.to_string())),
        };
        let me = status["repos"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|r| r["agents"].as_array().cloned().unwrap_or_default())
            .find(|a| {
                a["name"] == agent.name.as_str()
                    && a["path"] == agent.path.to_str().unwrap_or_default()
            });
        let checker = status["checker"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|g| g["repo"] == agent.repo.to_str().unwrap_or_default());
        let snap = checker.and_then(|g| {
            g["agents"]
                .as_array()?
                .iter()
                .find(|a| a["name"] == agent.name.as_str())
                .cloned()
        });
        let sharing = match snap.as_ref().map(|s| &s["sharing"]) {
            Some(s) if s["mode"] == "shared" => "shared".to_string(),
            Some(s) => format!("unshared ({})", s["reason"].as_str().unwrap_or("?")),
            None => "no snapshot yet (created on the first check)".into(),
        };
        let field = |k: &str| {
            me.as_ref()
                .and_then(|m| m[k].as_str())
                .unwrap_or("?")
                .to_string()
        };
        let running: String = me
            .as_ref()
            .and_then(|m| m["processes"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter(|p| p["state"] == "running")
            .map(|p| {
                format!(
                    "\nprocess {} ({}): {}",
                    p["id"],
                    p["kind"].as_str().unwrap_or("?"),
                    p["cmd"].as_str().unwrap_or("?")
                )
            })
            .collect();
        let memory = me
            .as_ref()
            .and_then(|m| m["memory_bytes"].as_u64())
            .unwrap_or(0)
            / 1048576;
        let budget = me
            .as_ref()
            .and_then(|m| m["budget"].as_str())
            .unwrap_or("ok")
            .to_string();
        let port = me
            .as_ref()
            .map(|m| m["port"].to_string())
            .unwrap_or_default();
        let typescript = checker
            .map(|g| format!(", TypeScript {}", g["typescript"].as_str().unwrap_or("?")))
            .unwrap_or_default();
        Ok(text(format!(
            "agent: {}\nworktree: {}\nbranch: {}\nstate: {}\nport: {port}\nmemory: {memory} MB (budget: {budget})\nchecker: {sharing}{typescript}{running}",
            agent.name,
            agent.path.display(),
            field("branch"),
            field("state"),
        )))
    }
}

impl Server {
    async fn check(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut body: Value,
        fmt: fn(&Value) -> String,
    ) -> CallToolResult {
        let agent = match self.resolve(ctx).await {
            Ok(a) => a,
            Err(e) => return failure(e.to_string()),
        };
        body["repo"] = json!(agent.repo);
        let path = format!("/v1/agents/{}/check", agent.name);
        match blocking(move || client::post(&path, body)).await {
            Ok(v) => text(fmt(&v) + &budget_note(&agent).await),
            Err(e) => failure(e.to_string()),
        }
    }

    async fn procs(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut body: Value,
        fmt: impl Fn(&Value) -> String,
    ) -> CallToolResult {
        let agent = match self.resolve(ctx).await {
            Ok(a) => a,
            Err(e) => return failure(e.to_string()),
        };
        body["repo"] = json!(agent.repo);
        let path = format!("/v1/agents/{}/procs", agent.name);
        match blocking(move || client::post(&path, body)).await {
            Ok(v) => text(fmt(&v) + &budget_note(&agent).await),
            Err(e) => failure(e.to_string()),
        }
    }

    // MCP roots are deprecated by SEP-2577 (protocol 2026-07-28) but still answered by clients
    // that support them; PLAN asks for them first. The working directory and --agent remain.
    #[allow(deprecated)]
    async fn resolve(&self, ctx: &RequestContext<RoleServer>) -> Result<Agent> {
        if let Some(a) = self.agent.get() {
            return Ok(a.clone());
        }
        let all = agents(&blocking(|| client::get("/v1/status")).await?);
        let found = 'found: {
            let roots_supported = ctx
                .peer
                .peer_info()
                .is_some_and(|i| i.capabilities.roots.is_some());
            if roots_supported && let Ok(r) = ctx.peer.list_roots().await {
                for root in r.roots {
                    if let Some(a) = file_uri_path(&root.uri).and_then(|p| containing(&all, &p)) {
                        break 'found Some(a);
                    }
                }
            }
            if let Some(a) = std::env::current_dir()
                .ok()
                .and_then(|cwd| containing(&all, &cwd))
            {
                break 'found Some(a);
            }
            let name = self
                .flag
                .clone()
                .or_else(|| std::env::var("STRATA_AGENT").ok());
            if let Some(name) = name {
                let named: Vec<_> = all.iter().filter(|a| a.name == name).collect();
                if named.len() > 1 {
                    return Err(Error::Refused(format!(
                        "several repos have an agent named {name:?}; start `strata mcp` inside its worktree instead"
                    )));
                }
                break 'found named.first().map(|a| (*a).clone());
            }
            None
        };
        let agent = found.ok_or_else(|| {
            Error::NotFound(
                "Strata cannot tell which agent is calling: no MCP root or working directory is inside a Strata worktree, and no --agent or STRATA_AGENT names one".into(),
            )
        })?;
        Ok(self.agent.get_or_init(|| async { agent }).await.clone())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Strata answers TypeScript questions for your worktree (errors, types, definitions, references, outlines) from a type checker shared with other agents. Prefer these tools to running tsc. strata_conflicts checks your changes against other agents' before either is merged.",
            )
    }
}

/// Serves MCP on stdio until the client disconnects.
pub async fn serve(agent: Option<String>) -> Result<()> {
    let io = |e: &dyn std::fmt::Display| Error::Io(std::io::Error::other(e.to_string()));
    let service = Server::new(agent)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| io(&e))?;
    service.waiting().await.map_err(|e| io(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            file_uri_path("file:///a%20b/c"),
            Some(PathBuf::from("/a b/c"))
        );
        assert_eq!(file_uri_path("https://x"), None);
    }

    #[test]
    fn picks_the_deepest_containing_worktree() {
        let a = |n: &str, p: &str| Agent {
            repo: "/r".into(),
            name: n.into(),
            path: p.into(),
        };
        let all = [
            a("outer", "/r/app"),
            a("nested", "/r/app/.claude/worktrees/x"),
        ];
        assert_eq!(
            containing(&all, Path::new("/r/app/.claude/worktrees/x/src"))
                .unwrap()
                .name,
            "nested"
        );
        assert_eq!(
            containing(&all, Path::new("/r/app/src")).unwrap().name,
            "outer"
        );
        assert!(containing(&all, Path::new("/r/app2")).is_none());
    }
}
