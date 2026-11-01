//! What `describe` returns for a command's schema, kept affordable for an agent (DECISIONS D71).
//!
//! Registered schemas are self-contained (no `$ref`s), which CLI generation and argument checks
//! rely on; their repetition is what makes them large. `describe` works on a view of them:
//!
//! 1. **Shared definitions** ([`share`]): the length type (over a thousand uses in the command
//!    set) and every other repeated subschema move to `$defs`. Standard JSON Schema, nothing lost.
//! 2. **Folding** ([`fold`]): when a schema stays over [`FOLD_LIMIT`], its largest arguments are
//!    folded: description, a signature of their fields (per variant for tagged unions) and how
//!    to ask for the rest. Rarely used and large arguments (`package_spec`, `stack`) are the ones
//!    that cost most and are needed least, and an agent that needs one wants one variant of it.
//! 3. **Detail** ([`at_path`]): `describe` with a `path` (`package_spec.qfn`, `pads.shape`)
//!    returns that part of the schema, itself shared and folded the same way. `full` returns
//!    the schema as registered.
//!
//! All deterministic: walks follow the schema's own (preserved) key order.

use serde_json::{Map, Value, json};

/// Size (serialized bytes) above which a command's schema is folded.
pub const FOLD_LIMIT: usize = 6_000;
/// Arguments smaller than this (serialized, shared) are never folded: most agents need them
/// (`pins` with its pin types), and the fold's own text would cost about as much.
const FOLD_MIN: usize = 2_500;
/// A repeated subschema smaller than this stays where it is.
const SHARE_MIN: usize = 100;
/// Largest number of definitions [`share`] creates.
const SHARE_MAX: usize = 64;
/// The length type's pattern ends with this list of units.
const LENGTH_UNITS: &str = "(nm|um|µm|mm|cm|m|mil|mils|thou|in|inch)";
/// Generic description of the length type.
const LENGTH_DESCRIPTION: &str = "Length with a mandatory unit";

fn len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

/// Calls `f` on every direct subschema of `s` (the positions where JSON Schema holds schemas, so
/// that a map of properties is never mistaken for a schema).
fn each_child(s: &mut Value, f: &mut dyn FnMut(&str, &mut Value)) {
    let Some(o) = s.as_object_mut() else { return };
    for key in ["properties", "$defs"] {
        if let Some(Value::Object(m)) = o.get_mut(key) {
            for (k, v) in m.iter_mut() {
                f(k, v);
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(v) = o.get_mut(key).filter(|v| v.is_object()) {
            f(key, v);
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(a)) = o.get_mut(key) {
            for v in a.iter_mut() {
                f(key, v);
            }
        }
    }
}

/// Moves the pattern of the length type to `$defs/length` wherever it appears, and drops the
/// generic description, which the definition carries. Returns the uses replaced.
fn share_length(s: &mut Value, pattern: &mut Option<String>) -> usize {
    let mut uses = 0;
    if let Some(o) = s.as_object_mut()
        && let Some(p) = o.get("pattern").and_then(Value::as_str).filter(|p| p.contains(LENGTH_UNITS))
    {
        if pattern.is_none() {
            *pattern = Some(p.to_string());
        }
        if pattern.as_deref() == Some(p) {
            o.remove("pattern");
            if o.get("description").and_then(Value::as_str).is_some_and(|d| d.starts_with(LENGTH_DESCRIPTION)) {
                o.remove("description");
            }
            o.insert("$ref".into(), json!("#/$defs/length"));
            uses += 1;
        }
    }
    each_child(s, &mut |_, c| uses += share_length(c, pattern));
    uses
}

/// Counts the subschemas under `s` (not `s` itself) by serialized form, with the name of the
/// position each was first seen at, in walk order.
fn count_children(s: &mut Value, counts: &mut indexmap::IndexMap<String, (usize, String)>) {
    each_child(s, &mut |key, c| {
        if let Ok(text) = serde_json::to_string(c) {
            counts.entry(text).or_insert((0, key.to_string())).0 += 1;
        }
        count_children(c, counts);
    });
}

/// Replaces every subschema equal to `text` under `s` with a reference to `name`.
fn replace_children(s: &mut Value, text: &str, name: &str) {
    each_child(s, &mut |_, c| {
        if serde_json::to_string(c).is_ok_and(|t| t == text) {
            *c = json!({"$ref": format!("#/$defs/{name}")});
        } else {
            replace_children(c, text, name);
        }
    });
}

/// The schema with the length type and repeated subschemas moved to `$defs`: the same schema,
/// in fewer bytes. A schema whose repetition does not pay stays as it is.
pub fn share(schema: &Value) -> Value {
    let mut s = schema.clone();
    let mut defs = Map::new();
    // The length type: only when used more than once, or the definition costs more than it saves.
    let mut probe = s.clone();
    let mut pattern = None;
    if share_length(&mut probe, &mut pattern) > 1
        && let Some(p) = pattern
    {
        s = probe;
        defs.insert(
            "length".into(),
            json!({"pattern": p, "description": "Length with a mandatory unit: nm, um, mm, cm, m, mil, in. \
                Examples: \"0.2mm\", \"8mil\", \"0.1in\"."}),
        );
    }
    for _ in 0..SHARE_MAX {
        let mut counts = indexmap::IndexMap::new();
        count_children(&mut s, &mut counts);
        // The repetition saving most; ties to the first seen.
        let mut best: Option<(usize, String, String)> = None;
        for (text, (n, key)) in &counts {
            if *n < 2 || text.len() < SHARE_MIN {
                continue;
            }
            let saved = (n - 1) * text.len();
            if best.as_ref().is_none_or(|b| saved > b.0) {
                best = Some((saved, text.clone(), key.clone()));
            }
        }
        let Some((_, text, key)) = best else { break };
        let base = match key.as_str() {
            "items" | "additionalProperties" | "anyOf" | "oneOf" | "allOf" | "prefixItems" => "shared".to_string(),
            k => k.to_string(),
        };
        let mut name = base.clone();
        let mut k = 2;
        while defs.contains_key(&name) {
            name = format!("{base}_{k}");
            k += 1;
        }
        replace_children(&mut s, &text, &name);
        let Ok(def) = serde_json::from_str(&text) else { break };
        defs.insert(name, def);
        // Definitions may repeat each other's parts: they are walked too from here on.
        if let Some(o) = s.as_object_mut() {
            let mut all = match o.remove("$defs") {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            all.append(&mut defs);
            o.insert("$defs".into(), Value::Object(all.clone()));
            defs = all;
        }
    }
    if !defs.is_empty()
        && let Some(o) = s.as_object_mut()
    {
        o.insert("$defs".into(), Value::Object(defs));
    }
    s
}

/// The non-null variants of a union (`anyOf`, `oneOf`), if `s` is one.
fn variants(s: &Value) -> Option<Vec<&Value>> {
    let v = s.get("anyOf").or_else(|| s.get("oneOf")).and_then(Value::as_array)?;
    Some(v.iter().filter(|x| x.get("type") != Some(&json!("null"))).collect())
}

/// The tag property of a tagged union's variants, if they have one.
fn union_tag(vs: &[&Value]) -> Option<String> {
    let props = vs.first()?.get("properties")?.as_object()?;
    props.keys().find(|k| vs.iter().all(|x| x["properties"][k.as_str()].get("const").is_some())).cloned()
}

/// `s` with the wrapper of an optional value or an array taken off, as far as they go.
fn unwrap_value(s: &Value) -> &Value {
    let mut cur = s;
    for _ in 0..8 {
        match variants(cur).as_deref() {
            Some([one]) => cur = one,
            Some(_) => return cur,
            None => match cur.get("items").filter(|i| i.is_object()) {
                Some(i) => cur = i,
                None => return cur,
            },
        }
    }
    cur
}

/// The field names of an object schema, required ones first as they stand, optional ones
/// marked `?`.
fn fields_of(s: &Value) -> Option<String> {
    let props = s.get("properties")?.as_object()?;
    let required: Vec<&str> = s
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let names: Vec<String> =
        props.keys().map(|k| if required.contains(&k.as_str()) { k.clone() } else { format!("{k}?") }).collect();
    Some(format!("{{{}}}", names.join(", ")))
}

/// What a folded schema still says: description, fields, and per variant of a union its fields,
/// each with the path that gives it in full.
fn folded(s: &Value, path: &str, command: &str) -> Value {
    let inner = unwrap_value(s);
    let mut o = Map::new();
    if let Some(d) = s.get("description").or_else(|| inner.get("description")) {
        o.insert("description".into(), d.clone());
    }
    let mut f = Map::new();
    if let Some(vs) = variants(inner) {
        let tag = union_tag(&vs);
        let mut list = Map::new();
        for (i, v) in vs.iter().enumerate() {
            let name = match &tag {
                Some(t) => v["properties"][t.as_str()]["const"].as_str().unwrap_or("").to_string(),
                None => i.to_string(),
            };
            let mut e = Map::new();
            if let Some(d) = v.get("description").and_then(Value::as_str) {
                // The first sentence; the detail has the rest.
                let first = d.split("\n\n").next().unwrap_or(d).replace('\n', " ");
                let first = first.split_once(". ").map_or(first.as_str(), |x| x.0);
                e.insert("description".into(), json!(first));
            }
            if let Some(x) = fields_of(v) {
                e.insert("fields".into(), json!(x));
            } else if let Some(c) = v.get("const") {
                e.insert("const".into(), c.clone());
            }
            list.insert(name, Value::Object(e));
        }
        if let Some(t) = tag {
            f.insert("tag".into(), json!(t));
        }
        f.insert("variants".into(), Value::Object(list));
        f.insert("detail".into(), json!(format!("describe {command} with path \"{path}.<variant>\"")));
    } else {
        if let Some(x) = fields_of(inner) {
            f.insert("fields".into(), json!(x));
        }
        f.insert("detail".into(), json!(format!("describe {command} with path \"{path}\"")));
    }
    if s.get("items").is_some() {
        f.insert("array".into(), json!(true));
    }
    o.insert("x-folded".into(), Value::Object(f));
    Value::Object(o)
}

/// The schema folded under [`FOLD_LIMIT`], shared; with the paths of the arguments folded, largest
/// first. A schema already under the limit once shared is not folded at all.
pub fn fold(schema: &Value, prefix: &str, command: &str) -> (Value, Vec<String>) {
    let mut cur = share(schema);
    let mut names = Vec::new();
    if len(&cur) <= FOLD_LIMIT {
        return (cur, names);
    }
    let mut work = schema.clone();
    let join = |k: &str| if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") };
    // Arguments, by what they cost once shared.
    let mut sizes: Vec<(usize, String)> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.iter().map(|(k, v)| (len(&share(v)), k.clone())).collect())
        .unwrap_or_default();
    sizes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (size, k) in sizes {
        if size < FOLD_MIN {
            break;
        }
        let path = join(&k);
        let sub = folded(&schema["properties"][k.as_str()], &path, command);
        work["properties"][k.as_str()] = sub;
        names.push(path);
        cur = share(&work);
        if len(&cur) <= FOLD_LIMIT {
            break;
        }
    }
    (cur, names)
}

/// The part of a schema at a dotted path: property names, `*` for a map's values (a step past an
/// array or an optional value needs nothing), a union's variant by its tag value or its index.
/// `Err` holds the step that fits nothing and what would fit there.
pub fn at_path<'a>(schema: &'a Value, path: &str) -> Result<&'a Value, (String, Vec<String>)> {
    let mut cur = schema;
    for raw in path.split('.').filter(|s| !s.is_empty()) {
        let step = raw.trim_end_matches("[]");
        let inner = unwrap_value(cur);
        if let Some(p) = inner.get("properties").and_then(|p| p.get(step)) {
            cur = p;
            continue;
        }
        if step == "*"
            && let Some(a) = inner.get("additionalProperties").filter(|a| a.is_object())
        {
            cur = a;
            continue;
        }
        if let Some(vs) = variants(inner) {
            let tag = union_tag(&vs);
            let hit = vs.iter().enumerate().find(|(i, v)| match &tag {
                Some(t) => v["properties"][t.as_str()]["const"].as_str() == Some(step),
                None => i.to_string() == step,
            });
            if let Some((_, v)) = hit {
                cur = v;
                continue;
            }
        }
        return Err((raw.to_string(), steps_at(inner)));
    }
    Ok(cur)
}

/// The steps a path can take from `s`.
fn steps_at(s: &Value) -> Vec<String> {
    let s = unwrap_value(s);
    if let Some(vs) = variants(s) {
        return match union_tag(&vs) {
            Some(t) => {
                vs.iter().filter_map(|v| v["properties"][t.as_str()]["const"].as_str().map(String::from)).collect()
            }
            None => (0..vs.len()).map(|i| i.to_string()).collect(),
        };
    }
    let mut out: Vec<String> =
        s.get("properties").and_then(Value::as_object).map(|p| p.keys().cloned().collect()).unwrap_or_default();
    if s.get("additionalProperties").is_some_and(Value::is_object) {
        out.push("*".into());
    }
    out
}

/// Text for one schema of a `describe` at a path: its description and fields, or its variants.
/// Fields are listed by name with their description; `label` gives a field's type.
pub fn path_text(schema: &Value, label: &dyn Fn(&Value) -> String) -> String {
    let inner = unwrap_value(schema);
    let mut out = Vec::new();
    if let Some(d) = schema.get("description").or_else(|| inner.get("description")).and_then(Value::as_str) {
        out.push(format!("  {d}"));
    }
    if let Some(vs) = variants(inner) {
        let tag = union_tag(&vs);
        for (i, v) in vs.iter().enumerate() {
            let name = tag
                .as_ref()
                .and_then(|t| v["properties"][t.as_str()]["const"].as_str().map(String::from))
                .unwrap_or_else(|| v.get("const").and_then(Value::as_str).map_or_else(|| i.to_string(), String::from));
            let d = v.get("description").and_then(Value::as_str).unwrap_or("");
            out.push(format!("  {name}: {}  {d}", fields_of(v).unwrap_or_default()).trim_end().to_string());
        }
    } else if let Some(props) = inner.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = inner
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for (k, v) in props {
            let req = if required.contains(&k.as_str()) { " (required)" } else { "" };
            let d = v.get("description").and_then(Value::as_str).unwrap_or("");
            out.push(format!("  {k}: {}{req}  {d}", label(v)).trim_end().to_string());
        }
    } else {
        out.push(format!("  type: {}", label(inner)));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves every `$ref` of a shared schema, dropping `$defs`: what a reader of it sees.
    fn expand(v: &Value, defs: &Value) -> Value {
        match v {
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, x) in o {
                    match k.as_str() {
                        "$defs" => {}
                        "$ref" => {
                            let name = x.as_str().unwrap().rsplit('/').next().unwrap();
                            if let Value::Object(d) = expand(&defs[name], defs) {
                                for (dk, dv) in d {
                                    out.entry(dk).or_insert(dv);
                                }
                            }
                        }
                        _ => {
                            out.insert(k.clone(), expand(x, defs));
                        }
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| expand(x, defs)).collect()),
            x => x.clone(),
        }
    }

    /// `v` with object keys sorted, for comparing schemas whatever their key order.
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&o[k]))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            x => x.clone(),
        }
    }

    fn length(desc: &str) -> Value {
        json!({"type": "string", "description": desc, "pattern": format!("^\\s*[-+]?[0-9]+\\s*{LENGTH_UNITS}\\s*$")})
    }

    #[test]
    fn share_is_lossless_and_smaller() {
        let shape = json!({"type": "object", "description": "A shape long enough to be worth sharing, really.",
            "properties": {"w": length("Width."), "h": length("Height."), "kind": {"const": "rect"}}});
        let s = json!({"type": "object", "properties": {
            "a": shape, "b": shape, "c": {"type": "array", "items": shape},
            "d": length("Length with a mandatory unit: nm, um."), "e": {"type": "string"},
        }});
        let v = share(&s);
        assert!(len(&v) < len(&s), "{v}");
        assert_eq!(v["$defs"]["length"]["description"].as_str().map(|d| d.starts_with(LENGTH_DESCRIPTION)), Some(true));
        assert_eq!(v["properties"]["a"], v["properties"]["b"], "{v}");
        // Resolved again, every argument is what it was (the generic length description comes
        // from the definition, in its full wording).
        let mut want = s.clone();
        want["properties"]["d"]["description"] = v["$defs"]["length"]["description"].clone();
        assert_eq!(sorted(&expand(&v, &v["$defs"])), sorted(&want));
        // The same schema gives the same view.
        assert_eq!(share(&s), v);
    }

    /// Over every schema of the command set: sharing loses nothing, and every folded argument's
    /// path gives it.
    #[test]
    fn every_command_schema_survives_sharing() {
        let r = crate::command::Registry::with_builtins();
        for e in r.iter() {
            for schema in [&e.input_schema, &e.output_schema] {
                let v = share(schema);
                assert_eq!(sorted(&expand(&v, &v["$defs"])), sorted(schema), "{}", e.name);
            }
            let (_, names) = fold(&e.input_schema, "", e.name);
            for path in names {
                assert!(at_path(&e.input_schema, &path).is_ok(), "{} {path}", e.name);
                assert!(e.describe_path(&path).is_ok(), "{} {path}", e.name);
            }
        }
    }

    #[test]
    fn nothing_to_share_stays_as_it_is() {
        let s = json!({"type": "object", "properties": {"path": {"type": "string", "description": "Where."}}});
        assert_eq!(share(&s), s);
        // A length used once: the definition would cost more than it saves.
        let s = json!({"type": "object", "properties": {"w": length("Width.")}});
        assert_eq!(share(&s), s);
    }

    fn big_union() -> Value {
        let pad = "x".repeat(3000);
        json!({"type": "object", "properties": {
            "id": {"type": "string", "description": "The id."},
            "spec": {"description": "The spec.", "oneOf": [
                {"type": "object", "description": pad.clone() + "1", "required": ["family"],
                    "properties": {"family": {"const": "qfn"}, "pitch": length("Pitch.")}},
                {"type": "object", "description": pad.clone() + "2", "required": ["family"],
                    "properties": {"family": {"const": "dip"}, "rows": {"type": "integer"}}},
                {"type": "null"},
            ]},
        }})
    }

    #[test]
    fn fold_keeps_names_and_says_how_to_get_the_rest() {
        let (v, names) = fold(&big_union(), "", "x.y");
        assert_eq!(names, ["spec"]);
        assert_eq!(v["properties"]["id"]["description"], "The id.");
        let f = &v["properties"]["spec"];
        assert_eq!(f["description"], "The spec.");
        assert_eq!(f["x-folded"]["tag"], "family");
        assert_eq!(f["x-folded"]["variants"]["qfn"]["fields"], "{family, pitch?}");
        assert!(f["x-folded"]["detail"].as_str().unwrap().contains("spec.<variant>"), "{f}");
    }

    #[test]
    fn small_schemas_are_not_folded() {
        let s = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let (v, names) = fold(&s, "", "x.y");
        assert!(names.is_empty());
        assert_eq!(v, s);
    }

    #[test]
    fn paths_reach_properties_variants_and_items() {
        let s = big_union();
        assert_eq!(at_path(&s, "spec.qfn.pitch").unwrap()["description"], "Pitch.");
        assert_eq!(at_path(&s, "spec.dip.rows").unwrap()["type"], "integer");
        let e = at_path(&s, "spec.bga").unwrap_err();
        assert_eq!(e.0, "bga");
        assert_eq!(e.1, ["qfn", "dip"]);
        let a = json!({"properties": {"pads": {"type": "array", "items": {"properties": {"w": {"type": "string"}}}},
            "m": {"type": "object", "additionalProperties": {"properties": {"q": {"type": "integer"}}}}}});
        assert_eq!(at_path(&a, "pads.w").unwrap()["type"], "string");
        assert_eq!(at_path(&a, "pads[].w").unwrap()["type"], "string");
        assert_eq!(at_path(&a, "m.*.q").unwrap()["type"], "integer");
    }
}
