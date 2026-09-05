//! What the shell's own app lets an agent do: look at what is wrong, and at
//! what has left the process.
//!
//! Both read; the system app changes nothing, so it offers nothing that
//! writes. They are the two panels it keeps about itself — the problems
//! list and the effect log — answered as rows instead of drawn, so a chat
//! can say *the send to Vera failed, here is why* without the person
//! having to go and look.

use kernel::effect::LOG;
use kernel::session::Session;
use kernel::time::fmt_date_long;
use kernel::tool::Tool;
use serde_json::{json, Value};

use super::effects::job_line;

/// How many rows of the log one call is worth. The ring is short and the
/// queue drains; past this the panel is the place.
const MAX_EFFECTS: i64 = 100;

/// What a call asks for when it says nothing.
const EFFECTS: i64 = 20;

/// The system app's tools, both read-only.
#[must_use]
pub fn all() -> Vec<Tool> {
    vec![
        Tool::new(
            "problems.list",
            "What is standing wrong right now: an account that cannot reach its \
             server, a send that failed, a bucket that cannot be read. These are \
             derived from the rows, never stored, so fixing the cause makes one \
             go away. Call this when the person asks why something is not \
             working.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            false,
            problems,
        ),
        Tool::new(
            "effects.recent",
            "The newest entries of the effect log: everything that left the \
             process — a folder listed, a letter sent, a file written — with \
             what it was, whether it worked, and what went wrong if it did not. \
             Call this to find out what actually happened, as opposed to what \
             the rows say now.",
            json!({
                "type": "object",
                "properties": {"n": {"type": "integer", "description": "how many to answer, 100 at most"}},
                "additionalProperties": false
            }),
            false,
            recent,
        ),
    ]
}

/// Every source asked, in the order the panel lists them.
fn problems(s: &mut Session, _input: &Value) -> Result<Value, String> {
    let problems: Vec<Value> = s
        .problems()
        .into_iter()
        .map(|p| {
            json!({
                "key": p.key,
                "label": p.label,
                "line": p.line,
                "detail": p.detail,
            })
        })
        .collect();
    Ok(json!({"problems": problems}))
}

/// The log's newest rows: the queue and the in-memory ring joined, as the
/// panel's own table reads them, so a job that never became a row is here
/// beside one that did.
fn recent(s: &mut Session, input: &Value) -> Result<Value, String> {
    let n = match input.get("n") {
        None | Some(Value::Null) => EFFECTS,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| "`n` must be an integer".to_string())?
            .clamp(1, MAX_EFFECTS),
    };
    let page = LOG.spec.page(LOG.tags, None, 0, n as usize);
    let rows = s.store().rows_sql_deps(
        "effect recent",
        "the newest effects, the queue and the ring together",
        &page.sql,
        &page.params,
        LOG.spec.deps,
        LOG.map,
    );
    let effects: Vec<Value> = rows
        .iter()
        .map(|j| {
            json!({
                "kind": j.kind,
                "describe": job_line(j),
                "status": j.status_line(),
                "error": j.error,
                "when": fmt_date_long(j.created),
            })
        })
        .collect();
    Ok(json!({"effects": effects}))
}

#[cfg(test)]
mod tests {
    use kernel::app::App;
    use kernel::caps::Clip;

    use super::super::SYSTEM;
    use super::*;

    static APPS: &[&dyn App] = &[&SYSTEM];

    fn call(s: &mut Session, name: &str, input: &Value) -> Result<Value, String> {
        let t = s
            .apps()
            .tool(name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .clone();
        t.check(input)?;
        (t.run)(s, input)
    }

    /// The two are the shell's own panels, answered as rows. Both read, so
    /// neither writes.
    #[test]
    fn the_shells_own_app_offers_two_readings_of_itself() {
        let s = Session::fake(APPS);
        let names: Vec<&str> = s
            .apps()
            .tools()
            .iter()
            .filter(|t| !t.name.starts_with("sql.") && !t.name.starts_with("panels."))
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["problems.list", "effects.recent"]);
        assert!(
            s.apps().tools().iter().filter(|t| t.writes).count() == 1,
            "only sql.write"
        );
    }

    /// Every source asked, in the shape the panel draws — and a store with
    /// no bucket has the one problem device sync knows how to have.
    #[test]
    fn the_problems_tool_answers_what_stands() {
        let mut s = Session::fake(APPS);
        let out = call(&mut s, "problems.list", &json!({})).expect("the problems");
        let rows = out["problems"].as_array().expect("problems").clone();
        assert_eq!(rows.len(), s.problems().len());
        for p in &rows {
            for key in ["key", "label", "line", "detail"] {
                assert!(p[key].is_string(), "{key} of {p}");
            }
        }
    }

    /// The log's newest rows, the queue and the in-memory ring together: an
    /// effect that ran at the call is here beside one that was filed.
    #[test]
    fn the_effects_tool_answers_the_newest_of_the_log() {
        let mut s = Session::fake(APPS);
        assert_eq!(
            call(&mut s, "effects.recent", &json!({})).expect("the log")["effects"],
            json!([]),
            "nothing has left the process yet"
        );
        s.world()
            .run(&Clip {
                text: "hello",
                what: "a line",
            })
            .expect("the clipboard took it");
        s.store().poll_external();

        let out = call(&mut s, "effects.recent", &json!({"n": 5})).expect("the log");
        let rows = out["effects"].as_array().expect("effects").clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["kind"], json!("clip"));
        assert_eq!(rows[0]["describe"], json!("copy a line (5 bytes)"));
        assert_eq!(rows[0]["status"], json!("done · in memory"));
        assert_eq!(rows[0]["error"], Value::Null);
        assert!(rows[0]["when"].as_str().is_some_and(|w| !w.is_empty()));

        // How many is a number, and a big one is the ceiling.
        assert!(call(&mut s, "effects.recent", &json!({"n": 10_000})).is_ok());
        assert_eq!(
            call(&mut s, "effects.recent", &json!({"n": "five"})).expect_err("not a number"),
            "`n` must be an integer"
        );
    }
}
