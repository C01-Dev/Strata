//! Checker answers as short text for agents (and `strata check`): `file:line:column` locations,
//! 1-based, paths relative to the worktree.

use serde_json::Value;

fn at(loc: &Value) -> String {
    format!(
        "{}:{}:{}",
        loc["file"].as_str().unwrap_or("?"),
        loc["line"],
        loc["column"]
    )
}

fn sharing_note(v: &Value) -> String {
    match v["sharing"]["reason"].as_str() {
        Some(reason) => format!("\n(answered from an unshared snapshot: {reason})"),
        None => String::new(),
    }
}

pub fn diagnostics(v: &Value) -> String {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let total = v["total"].as_u64().unwrap_or(items.len() as u64);
    if total == 0 {
        return format!("No problems.{}", sharing_note(v));
    }
    let mut out: Vec<String> = items
        .iter()
        .map(|d| {
            let category = d["category"].as_str().unwrap_or("error");
            format!(
                "{} {category} TS{}: {}",
                at(d),
                d["code"],
                d["message"].as_str().unwrap_or_default()
            )
        })
        .collect();
    let shown = if (items.len() as u64) < total {
        format!(", showing the first {}", items.len())
    } else {
        String::new()
    };
    out.push(format!(
        "{total} problem{}{shown}.{}",
        if total == 1 { "" } else { "s" },
        sharing_note(v)
    ));
    out.join("\n")
}

pub fn hover(v: &Value) -> String {
    v["text"].as_str().map_or_else(
        || "No type information at that position.".into(),
        String::from,
    )
}

pub fn locations(v: &Value) -> String {
    let locs: Vec<String> = v["locations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(at)
        .collect();
    if locs.is_empty() {
        "None found.".into()
    } else {
        locs.join("\n")
    }
}

pub fn symbols(v: &Value) -> String {
    fn walk(items: &Value, depth: usize, out: &mut Vec<String>) {
        for s in items.as_array().into_iter().flatten() {
            let kind = s["kind"]
                .as_str()
                .unwrap_or_default()
                .trim_end_matches("Declaration");
            out.push(format!(
                "{}{} ({kind}) {}:{}",
                "  ".repeat(depth),
                s["name"].as_str().unwrap_or("?"),
                s["loc"]["line"],
                s["loc"]["column"]
            ));
            walk(&s["children"], depth + 1, out);
        }
    }
    let mut out = vec![];
    walk(&v["symbols"], 0, &mut out);
    if out.is_empty() {
        "No declarations.".into()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats() {
        let d = json!({ "items": [{ "file": "src/b.ts", "line": 2, "column": 14, "code": 2322, "category": "error", "message": "Bad." }], "total": 3, "sharing": { "mode": "shared" } });
        assert_eq!(
            diagnostics(&d),
            "src/b.ts:2:14 error TS2322: Bad.\n3 problems, showing the first 1."
        );
        let none = json!({ "items": [], "total": 0, "sharing": { "mode": "unshared", "reason": "dependencies differ" } });
        assert_eq!(
            diagnostics(&none),
            "No problems.\n(answered from an unshared snapshot: dependencies differ)"
        );
        assert_eq!(hover(&Value::Null), "No type information at that position.");
        assert_eq!(
            locations(&json!({ "locations": [{ "file": "a.ts", "line": 1, "column": 2 }] })),
            "a.ts:1:2"
        );
        let syms = json!({ "symbols": [{ "name": "Box", "kind": "ClassDeclaration", "loc": { "line": 3, "column": 14 }, "children": [{ "name": "grow", "kind": "MethodDeclaration", "loc": { "line": 3, "column": 30 } }] }] });
        assert_eq!(symbols(&syms), "Box (Class) 3:14\n  grow (Method) 3:30");
    }
}
