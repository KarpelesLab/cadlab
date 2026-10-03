//! Canonical JSON writer: deterministic and diff-friendly.
//!
//! Small leaf records stay on one line, so moving a via changes one line:
//!
//! ```json
//! {
//!   "vias": [
//!     {"id": 812, "at": ["12.7mm", "8.4mm"], "net": "GND"},
//!     {"id": 813, "at": ["14.2mm", "8.4mm"], "net": "GND"}
//!   ]
//! }
//! ```
//!
//! Rules: the root is always expanded. A *leaf record* is written inline when it fits in
//! [`MAX_LINE`] columns: an array of scalars (a point), or an object whose values are scalars
//! or arrays of scalars (a via, a component). Anything holding records or points is expanded,
//! one element per line, so adding an element is a one-line diff. Key order is the order of the
//! input value (struct field order, or sorted for maps).

use serde_json::Value;

/// Maximum line width for inline containers.
pub const MAX_LINE: usize = 140;

const INDENT: &str = "  ";

/// Writes `value` in canonical form, with a trailing newline.
pub fn to_canonical_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, 0, true, &mut out);
    out.push('\n');
    out
}

fn write_value(v: &Value, depth: usize, root: bool, out: &mut String) {
    match v {
        Value::Array(items) if !items.is_empty() => {
            if !root && let Some(s) = inline(v, depth) {
                out.push_str(&s);
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                newline(depth + 1, out);
                write_value(item, depth + 1, false, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
            }
            newline(depth, out);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            if !root && let Some(s) = inline(v, depth) {
                out.push_str(&s);
                return;
            }
            out.push('{');
            let n = map.len();
            for (i, (k, item)) in map.iter().enumerate() {
                newline(depth + 1, out);
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                write_value(item, depth + 1, false, out);
                if i + 1 < n {
                    out.push(',');
                }
            }
            newline(depth, out);
            out.push('}');
        }
        _ => out.push_str(&compact(v)),
    }
}

fn newline(depth: usize, out: &mut String) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// The inline rendering of `v`, if `v` may be inlined at this depth.
fn inline(v: &Value, depth: usize) -> Option<String> {
    if !is_leaf_record(v) {
        return None;
    }
    let s = compact(v);
    // Account for indentation and a key prefix of reasonable size.
    (depth * INDENT.len() + s.len() <= MAX_LINE).then_some(s)
}

/// Arrays of scalars, and objects whose values are scalars or arrays of scalars.
fn is_leaf_record(v: &Value) -> bool {
    let scalar = |x: &Value| !matches!(x, Value::Array(_) | Value::Object(_));
    let scalar_array = |x: &Value| match x {
        Value::Array(a) => a.iter().all(scalar),
        Value::Object(o) => o.is_empty(),
        _ => true,
    };
    match v {
        Value::Array(items) => items.iter().all(scalar),
        Value::Object(map) => map.values().all(scalar_array),
        _ => true,
    }
}

/// Single-line rendering with `", "` and `": "` separators.
fn compact(v: &Value) -> String {
    match v {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(compact).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> =
                map.iter().map(|(k, v)| format!("{}: {}", Value::String(k.clone()), compact(v))).collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_one_per_line() {
        let v = json!({
            "vias": [
                {"id": 812, "at": ["12.7mm", "8.4mm"], "net": "GND"},
                {"id": 813, "at": ["14.2mm", "8.4mm"], "net": "GND"}
            ],
            "empty": [],
            "obj": {},
            "name": "x"
        });
        let expected = r#"{
  "vias": [
    {"id": 812, "at": ["12.7mm", "8.4mm"], "net": "GND"},
    {"id": 813, "at": ["14.2mm", "8.4mm"], "net": "GND"}
  ],
  "empty": [],
  "obj": {},
  "name": "x"
}
"#;
        assert_eq!(to_canonical_string(&v), expected);
    }

    #[test]
    fn single_record_list_still_expanded() {
        let v = json!({"vias": [{"id": 1}]});
        assert_eq!(to_canonical_string(&v), "{\n  \"vias\": [\n    {\"id\": 1}\n  ]\n}\n");
    }

    #[test]
    fn long_records_expand() {
        let long = "x".repeat(MAX_LINE + 20);
        let v = json!({"a": {"b": long}});
        let s = to_canonical_string(&v);
        assert!(s.contains("\"a\": {\n    \"b\": "), "{s}");
    }

    #[test]
    fn maps_of_records_expand() {
        let v = json!({"components": {"R1": {"id": 1, "part": "r"}, "R2": {"id": 2, "part": "r"}}});
        assert_eq!(
            to_canonical_string(&v),
            "{\n  \"components\": {\n    \"R1\": {\"id\": 1, \"part\": \"r\"},\n    \"R2\": {\"id\": 2, \"part\": \"r\"}\n  }\n}\n"
        );
    }

    #[test]
    fn empty_root() {
        assert_eq!(to_canonical_string(&json!({})), "{}\n");
    }

    #[test]
    fn roundtrips() {
        let v = json!({"a": [1, 2, {"b": [[1, 2], [3, 4]]}], "s": "q\"uote\n"});
        let s = to_canonical_string(&v);
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), v);
    }
}
