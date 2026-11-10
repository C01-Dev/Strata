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

/// " (and 168:32, 170:5)": other places in the file with the same clash.
pub fn also(c: &Value) -> String {
    let places: Vec<String> = c["also"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| format!("{}:{}", p[0], p[1]))
        .collect();
    if places.is_empty() {
        String::new()
    } else {
        format!(" (and {})", places.join(", "))
    }
}

fn sharing_note(v: &Value) -> String {
    match v["sharing"]["reason"].as_str() {
        Some(reason) => format!("\n(answered from an unshared snapshot: {reason})"),
        None => String::new(),
    }
}

/// Clashes between the caller's changes and other agents', one pair at a time.
fn clash_lines(pairs: &Value) -> Vec<String> {
    let mut out = vec![];
    for p in pairs.as_array().into_iter().flatten() {
        let with = p["with"].as_str().unwrap_or("?");
        if let Some(reason) = p["skipped"].as_str() {
            out.push(format!("Not checked against {with}'s changes: {reason}."));
        }
        let clashes = p["clashes"].as_array().cloned().unwrap_or_default();
        if !clashes.is_empty() {
            out.push(format!(
                "Your changes and {with}'s break each other once combined (neither is merged yet):"
            ));
        }
        for c in clashes {
            out.push(format!(
                "  {}{} error TS{}: {}",
                at(&c),
                also(&c),
                c["code"],
                c["message"].as_str().unwrap_or_default()
            ));
        }
        for f in p["unmerged"].as_array().into_iter().flatten() {
            out.push(format!(
                "You and {with} both changed {} and git cannot merge the two versions.",
                f.as_str().unwrap_or("?")
            ));
        }
    }
    out
}

/// `strata_conflicts`: every other agent with changes, checked against the caller's, and the
/// agents with no pair against the caller because one of the two has no changes yet.
pub fn conflicts(v: &Value) -> String {
    let mut lines = clash_lines(&v["pairs"]);
    let checked = v["pairs"].as_array().map_or(0, Vec::len);
    let unchecked: Vec<&str> = v["unchecked"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let (names, has, it) = match unchecked.as_slice() {
        [] => (String::new(), "", ""),
        [one] => (one.to_string(), "has", "it"),
        [rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "have", "them"),
    };
    let later = format!("A clash with {it} can still appear after this check.");
    // Without the shared checker, only git's merges were checked.
    let types_off = v["types_off"].as_str();
    if lines.is_empty() {
        lines.push(match (checked, types_off) {
            (0, _) if unchecked.is_empty() => {
                "Nothing to check: no other agent is working here.".into()
            }
            (0, _) => format!(
                "Nothing to check yet: {names} {has} no changes so far, or you have none. {later}"
            ),
            (1, None) => {
                "No clashes: your changes type-check together with the other agent's.".into()
            }
            (n, None) => format!(
                "No clashes: your changes type-check together with each of the {n} other agents'."
            ),
            (1, Some(_)) => "Git can merge your changes with the other agent's.".into(),
            (n, Some(_)) => {
                format!("Git can merge your changes with each of the {n} other agents'.")
            }
        });
    }
    if let Some(why) = types_off.filter(|_| checked > 0) {
        lines.push(format!("Types were not checked together: {why}."));
    }
    if checked > 0 && !unchecked.is_empty() {
        lines.push(format!(
            "Not checked yet: {names} {has} no changes so far. {later}"
        ));
    }
    lines.join("\n")
}

pub fn diagnostics(v: &Value) -> String {
    let clashes = clash_lines(&v["conflicts"]);
    let text = own_diagnostics(v);
    if clashes.is_empty() {
        text
    } else {
        format!("{text}\n{}", clashes.join("\n"))
    }
}

fn own_diagnostics(v: &Value) -> String {
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
        let clash = json!({ "items": [], "total": 0, "conflicts": [{ "with": "fix-login", "clashes": [{ "file": "src/c.ts", "line": 1, "column": 10, "code": 2305, "message": "No 'addOne'.", "also": [[4, 2], [9, 7]] }], "unmerged": ["src/a.ts"], "skipped": null }] });
        assert_eq!(
            diagnostics(&clash),
            "No problems.\nYour changes and fix-login's break each other once combined (neither is merged yet):\n  src/c.ts:1:10 (and 4:2, 9:7) error TS2305: No 'addOne'.\nYou and fix-login both changed src/a.ts and git cannot merge the two versions."
        );
        assert_eq!(
            conflicts(
                &json!({ "pairs": [{ "with": "a", "clashes": [], "unmerged": [], "skipped": null }, { "with": "b", "clashes": [], "unmerged": [], "skipped": null }] })
            ),
            "No clashes: your changes type-check together with each of the 2 other agents'."
        );
        assert_eq!(
            conflicts(
                &json!({ "pairs": [{ "with": "a", "clashes": [], "unmerged": [], "skipped": null }], "unchecked": ["b", "c", "d"] })
            ),
            "No clashes: your changes type-check together with the other agent's.\nNot checked yet: b, c and d have no changes so far. A clash with them can still appear after this check."
        );
        assert_eq!(
            conflicts(&json!({ "pairs": [], "unchecked": ["b", "c"] })),
            "Nothing to check yet: b and c have no changes so far, or you have none. A clash with them can still appear after this check."
        );
        assert_eq!(
            conflicts(&json!({ "pairs": [], "unchecked": [] })),
            "Nothing to check: no other agent is working here."
        );
        assert_eq!(
            conflicts(
                &json!({ "pairs": [{ "with": "a", "clashes": [], "unmerged": [], "skipped": "a's dependencies differ from the base's" }] })
            ),
            "Not checked against a's changes: a's dependencies differ from the base's."
        );
        let off = "TypeScript 5.9.3 is older than 7.1, so the shared checker is off";
        assert_eq!(
            conflicts(
                &json!({ "pairs": [{ "with": "a", "clashes": [], "unmerged": [], "skipped": null }], "unchecked": [], "types_off": off })
            ),
            format!(
                "Git can merge your changes with the other agent's.\nTypes were not checked together: {off}."
            )
        );
        assert_eq!(
            conflicts(
                &json!({ "pairs": [{ "with": "a", "clashes": [], "unmerged": ["README.md"], "skipped": null }], "unchecked": [], "types_off": off })
            ),
            format!(
                "You and a both changed README.md and git cannot merge the two versions.\nTypes were not checked together: {off}."
            )
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
