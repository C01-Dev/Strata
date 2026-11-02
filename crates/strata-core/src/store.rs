//! Durable state in SQLite (WAL). Schema = PLAN's data model; migrations are append-only
//! and tracked with `PRAGMA user_version`.

use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE repos (id INTEGER PRIMARY KEY, root TEXT UNIQUE NOT NULL, base_branch TEXT NOT NULL, ts_version TEXT, created_at INTEGER NOT NULL);
CREATE TABLE agents (id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL REFERENCES repos(id), name TEXT NOT NULL, path TEXT NOT NULL, branch TEXT NOT NULL, dep_key TEXT, seed_tier TEXT, budget_mb INTEGER, state TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE (repo_id, name));
CREATE TABLE snapshots (agent_id INTEGER PRIMARY KEY REFERENCES agents(id), group_key TEXT NOT NULL, layer_hash TEXT NOT NULL, changed_files INTEGER NOT NULL, removed_files INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE processes (id INTEGER PRIMARY KEY, agent_id INTEGER NOT NULL REFERENCES agents(id), kind TEXT NOT NULL, cmd TEXT NOT NULL, pid INTEGER, pid_start_time INTEGER, port INTEGER, state TEXT NOT NULL, log_path TEXT NOT NULL, started_at INTEGER, exit_code INTEGER);
CREATE TABLE ports (port INTEGER PRIMARY KEY, agent_id INTEGER REFERENCES agents(id), state TEXT NOT NULL);
CREATE TABLE donors (repo_id INTEGER NOT NULL REFERENCES repos(id), dep_key TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY (repo_id, dep_key));
CREATE TABLE events (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, agent_id INTEGER, kind TEXT NOT NULL, detail TEXT);
"#];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Repo {
    pub id: i64,
    pub root: PathBuf,
    pub base_branch: String,
    pub ts_version: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Agent {
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub state: AgentState,
    /// How dependencies got into the worktree: install, install-failed, none (not a JS repo);
    /// absent for adopted worktrees.
    pub seed_tier: Option<String>,
    pub created_at: i64,
}

/// `Creating` and `Removing` mark an operation in flight, so a restart after a crash knows
/// what to finish or undo (task 1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Creating,
    Ready,
    Removing,
    Orphaned,
}

impl AgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Removing => "removing",
            Self::Orphaned => "orphaned",
        }
    }

    fn parse(s: &str) -> rusqlite::Result<Self> {
        Ok(match s {
            "creating" => Self::Creating,
            "ready" => Self::Ready,
            "removing" => Self::Removing,
            "orphaned" => Self::Orphaned,
            _ => {
                return Err(rusqlite::Error::InvalidColumnType(
                    0,
                    s.into(),
                    rusqlite::types::Type::Text,
                ));
            }
        })
    }
}

/// A supervised process. `state`: running, stopping, stopped (by Strata), exited (by itself),
/// dead (found gone after a daemon restart).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Process {
    pub id: i64,
    pub agent_id: i64,
    pub kind: String,
    pub cmd: String,
    pub pid: Option<u32>,
    pub pid_start_time: Option<u64>,
    pub port: Option<u16>,
    pub state: String,
    pub log_path: PathBuf,
    pub started_at: Option<i64>,
    pub exit_code: Option<i32>,
}

const PROCESS_COLS: &str =
    "id, agent_id, kind, cmd, pid, pid_start_time, port, state, log_path, started_at, exit_code";

fn process(r: &Row) -> rusqlite::Result<Process> {
    Ok(Process {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        kind: r.get(2)?,
        cmd: r.get(3)?,
        pid: r.get(4)?,
        pid_start_time: r.get::<_, Option<i64>>(5)?.map(|t| t as u64),
        port: r.get(6)?,
        state: r.get(7)?,
        log_path: PathBuf::from(r.get::<_, String>(8)?),
        started_at: r.get(9)?,
        exit_code: r.get(10)?,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub at: i64,
    pub agent_id: Option<i64>,
    pub kind: String,
    pub detail: Option<String>,
}

pub struct Store {
    conn: Connection,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Refused(format!(
            "{} is not valid UTF-8; Strata needs UTF-8 paths",
            path.display()
        ))
    })
}

const AGENT_COLS: &str = "id, repo_id, name, path, branch, state, created_at, seed_tier";

fn agent(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        name: r.get(2)?,
        path: PathBuf::from(r.get::<_, String>(3)?),
        branch: r.get(4)?,
        state: AgentState::parse(&r.get::<_, String>(5)?)?,
        created_at: r.get(6)?,
        seed_tier: r.get(7)?,
    })
}

fn repo(r: &Row) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: r.get(0)?,
        root: PathBuf::from(r.get::<_, String>(1)?),
        base_branch: r.get(2)?,
        ts_version: r.get(3)?,
        created_at: r.get(4)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tx = conn.transaction()?;
        for sql in MIGRATIONS.iter().skip(version as usize) {
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
        tx.commit()?;
        Ok(Self { conn })
    }

    /// Registers a repo, or updates its base branch and TypeScript version.
    pub fn upsert_repo(
        &self,
        root: &Path,
        base_branch: &str,
        ts_version: Option<&str>,
    ) -> Result<Repo> {
        self.conn.execute(
            "INSERT INTO repos (root, base_branch, ts_version, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (root) DO UPDATE SET base_branch = excluded.base_branch, ts_version = excluded.ts_version",
            params![text(root)?, base_branch, ts_version, now()],
        )?;
        Ok(self.repo_by_root(root)?.expect("just upserted"))
    }

    pub fn repo_by_root(&self, root: &Path) -> Result<Option<Repo>> {
        let sql = "SELECT id, root, base_branch, ts_version, created_at FROM repos WHERE root = ?1";
        Ok(self.conn.query_row(sql, [text(root)?], repo).optional()?)
    }

    pub fn repos(&self) -> Result<Vec<Repo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root, base_branch, ts_version, created_at FROM repos ORDER BY id",
        )?;
        Ok(stmt.query_map([], repo)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn insert_agent(
        &self,
        repo_id: i64,
        name: &str,
        path: &Path,
        branch: &str,
        state: AgentState,
    ) -> Result<Agent> {
        let sql = "INSERT INTO agents (repo_id, name, path, branch, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
        match self.conn.execute(
            sql,
            params![repo_id, name, text(path)?, branch, state.as_str(), now()],
        ) {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                return Err(Error::Refused(format!(
                    "an agent named {name:?} already exists; pick another name"
                )));
            }
            r => r?,
        };
        Ok(self.agent(repo_id, name)?.expect("just inserted"))
    }

    pub fn agent(&self, repo_id: i64, name: &str) -> Result<Option<Agent>> {
        let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE repo_id = ?1 AND name = ?2");
        Ok(self
            .conn
            .query_row(&sql, params![repo_id, name], agent)
            .optional()?)
    }

    pub fn agent_by_id(&self, id: i64) -> Result<Option<Agent>> {
        let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id], agent).optional()?)
    }

    pub fn agent_by_path(&self, path: &Path) -> Result<Option<Agent>> {
        let sql = format!("SELECT {AGENT_COLS} FROM agents WHERE path = ?1");
        Ok(self.conn.query_row(&sql, [text(path)?], agent).optional()?)
    }

    pub fn agents(&self, repo_id: i64) -> Result<Vec<Agent>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {AGENT_COLS} FROM agents WHERE repo_id = ?1 ORDER BY id"
        ))?;
        Ok(stmt
            .query_map([repo_id], agent)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_agent_state(&self, id: i64, state: AgentState) -> Result<()> {
        self.conn.execute(
            "UPDATE agents SET state = ?1 WHERE id = ?2",
            params![state.as_str(), id],
        )?;
        Ok(())
    }

    /// Deletes the agent with its process records, ports and snapshot record.
    pub fn delete_agent(&self, id: i64) -> Result<()> {
        for table in ["processes", "ports", "snapshots"] {
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE agent_id = ?1"), [id])?;
        }
        self.conn
            .execute("DELETE FROM agents WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_agent_seed_tier(&self, id: i64, tier: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agents SET seed_tier = ?1 WHERE id = ?2",
            params![tier, id],
        )?;
        Ok(())
    }

    /// The agent switched branches inside its worktree.
    pub fn set_agent_branch(&self, id: i64, branch: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE agents SET branch = ?1 WHERE id = ?2",
            params![branch, id],
        )?;
        Ok(())
    }

    pub fn insert_process(
        &self,
        agent_id: i64,
        kind: &str,
        cmd: &str,
        log_path: &Path,
        port: Option<u16>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO processes (agent_id, kind, cmd, port, state, log_path) VALUES (?1, ?2, ?3, ?4, 'starting', ?5)",
            params![agent_id, kind, cmd, port, text(log_path)?],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_process_running(&self, id: i64, pid: u32, start: u64) -> Result<()> {
        let sql = "UPDATE processes SET pid = ?1, pid_start_time = ?2, state = 'running', started_at = ?3 WHERE id = ?4";
        self.conn
            .execute(sql, params![pid, start as i64, now(), id])?;
        Ok(())
    }

    pub fn set_process_state(&self, id: i64, state: &str, exit_code: Option<i32>) -> Result<()> {
        self.conn.execute(
            "UPDATE processes SET state = ?1, exit_code = COALESCE(?2, exit_code) WHERE id = ?3",
            params![state, exit_code, id],
        )?;
        Ok(())
    }

    pub fn set_process_port(&self, id: i64, port: u16) -> Result<()> {
        self.conn.execute(
            "UPDATE processes SET port = ?1 WHERE id = ?2",
            params![port, id],
        )?;
        Ok(())
    }

    pub fn process(&self, id: i64) -> Result<Option<Process>> {
        let sql = format!("SELECT {PROCESS_COLS} FROM processes WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id], process).optional()?)
    }

    /// An agent's processes, newest first.
    pub fn processes(&self, agent_id: i64) -> Result<Vec<Process>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROCESS_COLS} FROM processes WHERE agent_id = ?1 ORDER BY id DESC"
        ))?;
        Ok(stmt
            .query_map([agent_id], process)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Processes Strata believes are alive (starting, running or stopping).
    pub fn live_processes(&self) -> Result<Vec<Process>> {
        let sql = format!(
            "SELECT {PROCESS_COLS} FROM processes WHERE state IN ('starting', 'running', 'stopping') ORDER BY id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        Ok(stmt
            .query_map([], process)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Records `port` for the agent; false if another agent holds it.
    pub fn reserve_port(&self, port: u16, agent_id: i64) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO ports (port, agent_id, state) VALUES (?1, ?2, 'reserved')",
            params![port, agent_id],
        )?;
        Ok(n == 1)
    }

    pub fn agent_port(&self, agent_id: i64) -> Result<Option<u16>> {
        Ok(self
            .conn
            .query_row(
                "SELECT port FROM ports WHERE agent_id = ?1 ORDER BY port LIMIT 1",
                [agent_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn ports(&self) -> Result<Vec<u16>> {
        let mut stmt = self.conn.prepare("SELECT port FROM ports ORDER BY port")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn free_ports(&self, agent_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM ports WHERE agent_id = ?1", [agent_id])?;
        Ok(())
    }

    /// Appends to the event log. Events outlive their agent, so `agent_id` is not a foreign key.
    pub fn event(&self, agent_id: Option<i64>, kind: &str, detail: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (at, agent_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
            params![now(), agent_id, kind, detail],
        )?;
        Ok(())
    }

    /// Newest first.
    pub fn events(&self, limit: u32) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare("SELECT at, agent_id, kind, detail FROM events ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit], |r| {
            Ok(Event {
                at: r.get(0)?,
                agent_id: r.get(1)?,
                kind: r.get(2)?,
                detail: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    #[test]
    fn migrations_run_once_and_reopen_keeps_data() {
        let (dir, s) = store();
        s.upsert_repo(Path::new("/src/app"), "main", Some("7.1.0"))
            .unwrap();
        drop(s);
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        assert_eq!(s.repos().unwrap().len(), 1);
        let version: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let mode: String = s
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn upsert_repo_updates_in_place() {
        let (_dir, s) = store();
        let a = s.upsert_repo(Path::new("/src/app"), "main", None).unwrap();
        let b = s
            .upsert_repo(Path::new("/src/app"), "trunk", Some("7.1.0"))
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.base_branch, "trunk");
        assert_eq!(b.ts_version.as_deref(), Some("7.1.0"));
        assert_eq!(s.repo_by_root(Path::new("/src/other")).unwrap(), None);
    }

    #[test]
    fn agent_lifecycle() {
        let (_dir, s) = store();
        let repo = s.upsert_repo(Path::new("/src/app"), "main", None).unwrap();
        let a = s
            .insert_agent(
                repo.id,
                "fix-login",
                Path::new("/src/.strata/app/fix-login"),
                "fix-login",
                AgentState::Creating,
            )
            .unwrap();
        assert_eq!(a.state, AgentState::Creating);
        s.set_agent_state(a.id, AgentState::Ready).unwrap();
        assert_eq!(
            s.agent(repo.id, "fix-login").unwrap().unwrap().state,
            AgentState::Ready
        );
        assert_eq!(s.agent_by_path(&a.path).unwrap().unwrap().id, a.id);
        assert_eq!(s.agents(repo.id).unwrap().len(), 1);
        let err = s
            .insert_agent(
                repo.id,
                "fix-login",
                Path::new("/elsewhere"),
                "b",
                AgentState::Ready,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{err}");
        s.delete_agent(a.id).unwrap();
        assert_eq!(s.agent(repo.id, "fix-login").unwrap(), None);
    }

    #[test]
    fn processes_and_ports() {
        let (_dir, s) = store();
        let repo = s.upsert_repo(Path::new("/src/app"), "main", None).unwrap();
        let a = s
            .insert_agent(repo.id, "a", Path::new("/w/a"), "a", AgentState::Ready)
            .unwrap();
        let b = s
            .insert_agent(repo.id, "b", Path::new("/w/b"), "b", AgentState::Ready)
            .unwrap();
        assert!(s.reserve_port(4100, a.id).unwrap());
        assert!(!s.reserve_port(4100, b.id).unwrap(), "one owner per port");
        assert_eq!(s.agent_port(a.id).unwrap(), Some(4100));
        let id = s
            .insert_process(
                a.id,
                "dev",
                "next dev",
                Path::new("/h/logs/a/dev.log"),
                Some(4100),
            )
            .unwrap();
        s.set_process_running(id, 123, 456).unwrap();
        assert_eq!(s.live_processes().unwrap().len(), 1);
        s.set_process_state(id, "exited", Some(1)).unwrap();
        let p = s.process(id).unwrap().unwrap();
        assert_eq!(
            (p.pid, p.pid_start_time, p.state.as_str(), p.exit_code),
            (Some(123), Some(456), "exited", Some(1))
        );
        assert!(s.live_processes().unwrap().is_empty());
        s.delete_agent(a.id).unwrap();
        assert_eq!(
            s.ports().unwrap(),
            Vec::<u16>::new(),
            "ports go with the agent"
        );
        assert!(s.process(id).unwrap().is_none());
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let (_dir, s) = store();
        assert!(
            s.insert_agent(42, "x", Path::new("/x"), "x", AgentState::Ready)
                .is_err()
        );
    }

    #[test]
    fn events_newest_first() {
        let (_dir, s) = store();
        s.event(None, "repo.registered", "/src/app").unwrap();
        s.event(Some(1), "agent.created", "fix-login").unwrap();
        let events = s.events(10).unwrap();
        assert_eq!(
            events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["agent.created", "repo.registered"]
        );
        assert_eq!(events[0].agent_id, Some(1));
    }
}
