//! What an app lets an agent do, by name.
//!
//! A tool is the verb's own code path over ids instead of over a cursor, so a
//! tool and the button that does the same thing cannot disagree, and undo
//! works the same for both — a call is one [`Session::act`], labelled by the
//! tool, so `cmd+z` takes it back like any other action. The kernel names the
//! type and collects the list ([`Apps::tools`](crate::app::Apps::tools)); the
//! apps fill it and the chat runs a call by name.
//!
//! Undo is the net for nearly all of them, which is why nothing asks the
//! person first. [`Tool::asks`] is the exception: a call of a tool that
//! cannot be taken back — a send, a delete, a raw write — waits on its own
//! card until the person allows or refuses it.
//!
//! The schema is the other half of the contract. A model's arguments are a
//! claim, not a promise: [`Tool::check`] reads them against the tool's own
//! JSON Schema before `run` sees them, and its refusal is a sentence the
//! model can act on — the key that is missing, or the type that was wanted.

use serde_json::Value;

use crate::session::Session;

/// One thing an app lets an agent do, by name.
#[derive(Clone)]
pub struct Tool {
    /// Stable, prefixed with the app id (`mail.archive`, `files.rename`).
    /// Never renamed once a chat has used it.
    pub name: &'static str,
    /// For the model: what it does and *when* to call it.
    pub description: &'static str,
    /// JSON Schema for the input, `additionalProperties: false`; sent as
    /// the function's `parameters`. The arguments are checked against it on
    /// arrival — required keys, types — before `run` sees them.
    pub input: Value,
    /// Whether the world changes. The same word as an effect's; it is what
    /// the card's look and the log key on. It is **not** what the gate keys
    /// on: see [`Tool::asks`].
    pub writes: bool,
    /// Whether the call waits for the person's word before it runs: what
    /// cannot be undone, or leaves the machine — a send, a delete, a raw
    /// write. Every such call is a card that waits; `writes` alone does not
    /// ask, because a rename or an archive is one undo away.
    pub asks: bool,
    /// The whole behaviour, on the UI thread, with the session: one `act`
    /// per call, labelled by the tool, so it is one undo.
    pub run: fn(&mut Session, &Value) -> Result<Value, String>,
}

impl Tool {
    /// A tool from its parts, so an app declares one in a single
    /// expression.
    #[must_use]
    pub fn new(
        name: &'static str,
        description: &'static str,
        input: Value,
        writes: bool,
        run: fn(&mut Session, &Value) -> Result<Value, String>,
    ) -> Tool {
        Tool {
            name,
            description,
            input,
            writes,
            asks: false,
            run,
        }
    }

    /// The same tool, asking first: a call of it waits on the person's
    /// word — *allow* or *refuse* on its own card — rather than running the
    /// moment it arrives. What earns it is a thing undo cannot take back or
    /// that has already left the machine.
    #[must_use]
    pub fn asking(mut self) -> Tool {
        self.asks = true;
        self
    }

    /// Reads the arguments a call arrived with against [`Tool::input`].
    ///
    /// What it holds the model to: the input is an object; every `required`
    /// key is there; each declared property has the `type` it was promised
    /// (`integer` means a whole number, and a union such as
    /// `["string", "null"]` takes either), is one of the values an `enum`
    /// lists, and, where a property is itself an object or an array of
    /// them, the same again inside; and, under
    /// `additionalProperties: false`, no key nobody declared.
    ///
    /// A schema that says nothing about a key says nothing about it here
    /// either: this is a gate on the obvious mistakes, not a validator.
    ///
    /// # Errors
    ///
    /// The first thing wrong, in a sentence naming the key and what was
    /// expected: *missing `to`*, *`path` must be a string*, *unknown key
    /// `foo`*.
    pub fn check(&self, input: &Value) -> Result<(), String> {
        check_object(&self.input, input, "")
    }
}

/// One object against its schema: the required keys, the unknown ones, and
/// each declared property's own reading. `at` is the key path so far, empty
/// at the top, so a nested complaint still names the key a person sees.
fn check_object(schema: &Value, value: &Value, at: &str) -> Result<(), String> {
    let Some(obj) = value.as_object() else {
        return Err(if at.is_empty() {
            "the input must be an object".to_string()
        } else {
            format!("`{at}` must be an object")
        });
    };
    let props = schema.get("properties").and_then(Value::as_object);
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !obj.contains_key(key) {
                return Err(format!("missing `{}`", named(at, key)));
            }
        }
    }
    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for key in obj.keys() {
            if props.is_none_or(|p| !p.contains_key(key)) {
                return Err(format!("unknown key `{}`", named(at, key)));
            }
        }
    }
    let Some(props) = props else {
        return Ok(());
    };
    for (key, declared) in props {
        if let Some(v) = obj.get(key) {
            check_value(declared, v, &named(at, key))?;
        }
    }
    Ok(())
}

/// One value against the schema declared for it.
fn check_value(declared: &Value, value: &Value, at: &str) -> Result<(), String> {
    if let Some(types) = declared_types(declared) {
        if !types.iter().any(|t| is_a(t, value)) {
            return Err(format!("`{at}` must be {}", wanted(&types)));
        }
        // One level down is the whole of it in practice, but an object
        // inside an object is the same question asked again.
        if value.is_object() && declared.get("properties").is_some() {
            check_object(declared, value, at)?;
        }
        if let (Some(items), Some(list)) = (declared.get("items"), value.as_array()) {
            for (i, v) in list.iter().enumerate() {
                check_value(items, v, &format!("{at}[{i}]"))?;
            }
        }
    }
    if let Some(allowed) = declared.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let list: Vec<String> = allowed.iter().map(show).collect();
            return Err(format!("`{at}` must be one of: {}", list.join(", ")));
        }
    }
    Ok(())
}

/// `type` as a list, whether it was written as one name or as a union.
/// `None` where the schema declares no type, which is a schema saying the
/// value may be anything.
fn declared_types(declared: &Value) -> Option<Vec<&str>> {
    match declared.get("type")? {
        Value::String(one) => Some(vec![one.as_str()]),
        Value::Array(many) => Some(many.iter().filter_map(Value::as_str).collect()),
        _ => None,
    }
}

/// Whether a value is of the JSON type this name spells. A name this
/// checker does not know is not a refusal: the schema is the app's, and it
/// may say more than the gate reads.
fn is_a(name: &str, value: &Value) -> bool {
    match name {
        "string" => value.is_string(),
        // A JSON integer, not a number that happens to be whole: `2.0` is
        // the model saying something else.
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// What the sentence says was wanted: *a string*, *an array*, *a string or
/// null*.
fn wanted(types: &[&str]) -> String {
    let words: Vec<String> = types
        .iter()
        .map(|t| match *t {
            "null" => "null".to_string(),
            "integer" | "object" | "array" => format!("an {t}"),
            other => format!("a {other}"),
        })
        .collect();
    words.join(" or ")
}

/// One value of an `enum`, as a person reads it: a string bare, anything
/// else as its JSON.
fn show(v: &Value) -> String {
    match v.as_str() {
        Some(s) => s.to_string(),
        None => v.to_string(),
    }
}

/// The key path a complaint names.
fn named(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nothing(_s: &mut Session, _input: &Value) -> Result<Value, String> {
        Ok(Value::Null)
    }

    /// A tool over the shapes the checker has words for.
    fn rename() -> Tool {
        Tool::new(
            "test.rename",
            "renames a file",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "to": {"type": "string"},
                    "keep": {"type": "boolean"},
                    "times": {"type": "integer"},
                    "ratio": {"type": "number"},
                    "note": {"type": ["string", "null"]},
                    "how": {"type": "string", "enum": ["copy", "move"]},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "where": {
                        "type": "object",
                        "properties": {"dir": {"type": "string"}},
                        "required": ["dir"],
                        "additionalProperties": false
                    }
                },
                "required": ["path", "to"],
                "additionalProperties": false
            }),
            true,
            nothing,
        )
    }

    #[test]
    fn a_tool_asks_for_the_keys_its_schema_requires() {
        let t = rename();
        assert_eq!(t.check(&json!({"path": "a", "to": "b"})), Ok(()));
        assert_eq!(
            t.check(&json!({"path": "a"})),
            Err("missing `to`".to_string())
        );
        assert_eq!(
            t.check(&json!({})),
            Err("missing `path`".to_string()),
            "the first thing wrong, not a list of them"
        );
    }

    #[test]
    fn a_tool_refuses_a_key_nobody_declared() {
        assert_eq!(
            rename().check(&json!({"path": "a", "to": "b", "foo": 1})),
            Err("unknown key `foo`".to_string())
        );
    }

    #[test]
    fn each_declared_type_is_read() {
        let t = rename();
        assert_eq!(
            t.check(&json!({"path": 7, "to": "b"})),
            Err("`path` must be a string".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "keep": "yes"})),
            Err("`keep` must be a boolean".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "tags": "one"})),
            Err("`tags` must be an array".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "tags": ["one", 2]})),
            Err("`tags[1]` must be a string".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "where": "home"})),
            Err("`where` must be an object".to_string())
        );
    }

    #[test]
    fn an_integer_is_a_whole_number_and_a_number_is_any() {
        let t = rename();
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "times": 3})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "times": 3.5})),
            Err("`times` must be an integer".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "times": 2.0})),
            Err("`times` must be an integer".to_string()),
            "a float that lands on a whole number is still a float"
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "ratio": 3.5})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "ratio": 3})),
            Ok(())
        );
    }

    #[test]
    fn a_union_takes_either_of_its_types() {
        let t = rename();
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "note": "hi"})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "note": null})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "note": 3})),
            Err("`note` must be a string or null".to_string())
        );
    }

    #[test]
    fn an_enum_takes_only_what_it_lists() {
        let t = rename();
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "how": "move"})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "how": "burn"})),
            Err("`how` must be one of: copy, move".to_string())
        );
    }

    #[test]
    fn an_object_inside_is_read_the_same_way() {
        let t = rename();
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "where": {"dir": "~"}})),
            Ok(())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "where": {}})),
            Err("missing `where.dir`".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "where": {"dir": 1}})),
            Err("`where.dir` must be a string".to_string())
        );
        assert_eq!(
            t.check(&json!({"path": "a", "to": "b", "where": {"dir": "~", "x": 1}})),
            Err("unknown key `where.x`".to_string())
        );
    }

    #[test]
    fn the_arguments_themselves_must_be_an_object() {
        let t = rename();
        assert_eq!(
            t.check(&json!("path=a")),
            Err("the input must be an object".to_string())
        );
        assert_eq!(
            t.check(&Value::Null),
            Err("the input must be an object".to_string())
        );
    }

    #[test]
    fn a_tool_with_no_schema_of_its_own_takes_any_object() {
        let t = Tool::new("test.look", "looks", json!({}), false, nothing);
        assert_eq!(t.check(&json!({"anything": [1, 2]})), Ok(()));
        assert_eq!(
            t.check(&json!([])),
            Err("the input must be an object".to_string())
        );
    }

    #[test]
    fn a_tool_clones_with_its_schema() {
        let t = rename();
        let c = t.clone();
        assert_eq!(c.name, t.name);
        assert_eq!(c.input, t.input);
        assert!(c.writes);
    }

    #[test]
    fn a_tool_runs_on_arrival_unless_it_says_it_asks() {
        let plain = rename();
        assert!(!plain.asks, "a rename is one undo away, so it does not ask");
        let asking = rename().asking();
        assert!(asking.asks);
        assert!(asking.writes, "and it is still a write");
        assert!(asking.clone().asks, "which travels with the clone");
    }
}
