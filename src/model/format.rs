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
//! Rules: the root is always expanded; a container is written inline when it fits in
//! [`MAX_LINE`] columns and holds no array of containers (lists of records or points always
//! get one element per line, so adding one is a one-line diff). Key order is the order of the
//! input value (struct field order, or sorted for maps).

use serde_json::Value;

/// Maximum line width for inline containers.
pub const MAX_LINE: usize = 100;

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
    if has_container_array(v) {
        return None;
    }
    let s = compact(v);
    // Account for indentation and a key prefix of reasonable size.
    (depth * INDENT.len() + s.len() <= MAX_LINE).then_some(s)
}

/// Whether `v` contains (at any depth, including itself) a non-empty array whose elements
/// include a non-empty container.
fn has_container_array(v: &Value) -> bool {
    match v {
        Value::Array(items) => {
            items.iter().any(|i| match i {
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => false,
            }) || items.iter().any(has_container_array)
        }
        Value::Object(map) => map.values().any(has_container_array),
        _ => false,
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
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), compact(v)))
                .collect();
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
        assert_eq!(
            to_canonical_string(&v),
            "{\n  \"vias\": [\n    {\"id\": 1}\n  ]\n}\n"
        );
    }

    #[test]
    fn long_records_expand() {
        let long = "x".repeat(120);
        let v = json!({"a": {"b": long}});
        let s = to_canonical_string(&v);
        assert!(s.contains("\"a\": {\n    \"b\": "), "{s}");
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
