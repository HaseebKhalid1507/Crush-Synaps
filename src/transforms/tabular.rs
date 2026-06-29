//! Tabular fold — JSON array-of-objects → schema + CSV.
//!
//! A JSON array of objects repeats every key on every element. Folding to a
//! single header row plus CSV-style body writes each key once. Works on a
//! top-level array, OR on the single largest array-of-objects nested inside an
//! object tree (`{"packages":[…]}`, kubectl `.items`, `gh api`, docker inspect).
//!
//! **Meaning-preserving, not byte-lossless:** an LLM reads the table faithfully,
//! but the transform is one-way — JSON `null` and `""` both render as an empty
//! cell, `true`/`"true"`/`1` collapse, per-row key order becomes union order.
//! Use [`super::columnar`] when exact round-trip matters.

use crate::markers;
use serde_json::Value;
use std::collections::HashSet;

/// Max recursion depth when searching for a nested array-of-objects.
const MAX_DEPTH: usize = 8;

/// Fold a JSON array-of-objects (top-level or the largest nested one) into a
/// CSV-style table. `None` when there's no such array or folding wouldn't shrink.
#[must_use]
pub fn fold(s: &str) -> Option<String> {
    let value: Value = serde_json::from_str(s.trim()).ok()?;

    // Top-level array-of-objects: fold directly to a bare table.
    if let Some(arr) = value.as_array() {
        return build_table(arr);
    }

    // Otherwise: find the largest nested array-of-objects and fold it in place,
    // leaving the surrounding structure intact.
    let target = largest_aoo_size(&value, 0);
    if target == 0 {
        return None;
    }
    let mut root = value;
    if replace_largest(&mut root, target, 0) {
        let out = serde_json::to_string(&root).ok()?;
        return Some(out);
    }
    None
}

/// Build the table string for an array of ≥2 objects, or `None`.
fn build_table(arr: &[Value]) -> Option<String> {
    if arr.len() < 2 || !arr.iter().all(Value::is_object) {
        return None;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut columns: Vec<&str> = Vec::new();
    for obj in arr {
        for key in obj.as_object().unwrap().keys() {
            if seen.insert(key.as_str()) {
                columns.push(key.as_str());
            }
        }
    }
    if columns.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(64);
    out.push_str(markers::TABLE);
    out.push(' ');
    out.push_str(&arr.len().to_string());
    out.push_str(" rows\n");
    write_csv_row(&mut out, columns.iter().copied());
    out.push('\n');
    for obj in arr {
        let map = obj.as_object().unwrap();
        write_csv_row(&mut out, columns.iter().map(|col| cell(map.get(*col))));
        out.push('\n');
    }
    out.pop();
    Some(out)
}

fn is_aoo(a: &[Value]) -> bool {
    a.len() >= 2 && a.iter().all(Value::is_object)
}

/// Serialized byte size of the largest array-of-objects anywhere in the tree.
fn largest_aoo_size(v: &Value, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    let mut best = 0;
    match v {
        Value::Array(a) => {
            if is_aoo(a) {
                best = serde_json::to_string(v).map(|s| s.len()).unwrap_or(0);
            }
            for e in a {
                best = best.max(largest_aoo_size(e, depth + 1));
            }
        }
        Value::Object(m) => {
            for e in m.values() {
                best = best.max(largest_aoo_size(e, depth + 1));
            }
        }
        _ => {}
    }
    best
}

/// Replace the first array-of-objects whose serialized size equals `target` with
/// its folded table (as a JSON string). Returns whether a replacement happened.
fn replace_largest(v: &mut Value, target: usize, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    // Is THIS node the target array-of-objects?
    if let Value::Array(a) = v {
        if is_aoo(a) && serde_json::to_string(&*a).map(|s| s.len()).unwrap_or(0) == target {
            if let Some(folded) = build_table(a) {
                *v = Value::String(folded);
                return true;
            }
        }
    }
    // Otherwise recurse into children.
    match v {
        Value::Array(a) => a.iter_mut().any(|e| replace_largest(e, target, depth + 1)),
        Value::Object(m) => m
            .values_mut()
            .any(|e| replace_largest(e, target, depth + 1)),
        _ => false,
    }
}

/// Render an optional JSON value as a CSV cell's pre-escape text.
fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Write CSV-escaped, comma-joined fields directly into `out`.
fn write_csv_row<S: AsRef<str>>(out: &mut String, fields: impl Iterator<Item = S>) {
    let mut first = true;
    for f in fields {
        if !first {
            out.push(',');
        }
        write_escaped(out, f.as_ref());
        first = false;
    }
}

/// RFC4180-style escaping written straight into `out`.
fn write_escaped(out: &mut String, f: &str) {
    if f.contains([',', '"', '\n', '\r']) {
        out.push('"');
        for ch in f.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_top_level_array_of_objects() {
        let input = r#"[
            {"name":"alice","age":30,"city":"nyc"},
            {"name":"bob","age":25,"city":"sf"},
            {"name":"carol","age":41,"city":"la"}
        ]"#;
        let out = fold(input).expect("should fold");
        assert!(out.len() < input.len());
        assert!(out.contains("name,age,city"));
        assert!(out.contains("alice,30,nyc"));
        assert!(out.contains("@crush/1.table 3 rows"));
    }

    #[test]
    fn folds_largest_nested_array_in_an_object() {
        let input = r#"{"version":1,"packages":[
            {"name":"serde","ver":"1.0","id":"a"},
            {"name":"tokio","ver":"1.2","id":"b"},
            {"name":"clap","ver":"4.0","id":"c"},
            {"name":"anyhow","ver":"1.0","id":"d"}
        ],"workspace":true}"#;
        let out = fold(input).expect("should fold nested");
        assert!(out.contains("@crush/1.table 4 rows"));
        // surrounding structure preserved
        assert!(out.contains("\"version\":1"));
        assert!(out.contains("\"workspace\":true"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn picks_the_largest_array_when_several_nest() {
        let input = r#"{"small":[{"a":1},{"a":2}],"big":[
            {"k":"xxxxxxxxxx","v":"yyyyyyyyyy"},
            {"k":"xxxxxxxxxx","v":"yyyyyyyyyy"},
            {"k":"xxxxxxxxxx","v":"yyyyyyyyyy"},
            {"k":"xxxxxxxxxx","v":"yyyyyyyyyy"}
        ]}"#;
        let out = fold(input).unwrap();
        // the big array folds; the small one stays raw JSON
        assert!(out.contains("@crush/1.table 4 rows"));
        assert!(out.contains(r#""small":[{"a":1},{"a":2}]"#));
    }

    #[test]
    fn handles_missing_keys_with_empty_cells() {
        let input = r#"[{"a":1,"b":2},{"a":3},{"b":4,"c":5}]"#;
        let out = fold(input).unwrap();
        assert!(out.contains("a,b,c"));
        assert!(out.contains("3,,"));
    }

    #[test]
    fn escapes_commas_and_quotes() {
        let input = r#"[{"x":"a,b"},{"x":"he said \"hi\""}]"#;
        let out = fold(input).unwrap();
        assert!(out.contains("\"a,b\""));
        assert!(out.contains("\"he said \"\"hi\"\"\""));
    }

    #[test]
    fn rejects_non_array_without_nested_objects() {
        assert!(fold(r#"{"a":1}"#).is_none());
        assert!(fold("not json at all").is_none());
        assert!(fold(r#"[1,2,3]"#).is_none());
    }

    #[test]
    fn rejects_single_element_array() {
        assert!(fold(r#"[{"a":1}]"#).is_none());
    }
}
