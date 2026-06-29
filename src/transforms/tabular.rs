//! Tabular fold — JSON array-of-objects → schema + CSV.
//!
//! A JSON array of objects repeats every key on every element. Folding to a
//! single header row plus CSV-style body writes each key once.
//!
//! **Meaning-preserving, not byte-lossless:** an LLM reads the table faithfully,
//! but the transform is one-way — JSON `null` and `""` both render as an empty
//! cell, `true`/`"true"`/`1` collapse to the same text, and per-row key order is
//! replaced by the union order. Use [`super::columnar`] when exact round-trip
//! matters; this path optimises for model readability.

use crate::markers;
use serde_json::Value;
use std::collections::HashSet;

/// Fold a JSON array-of-objects into `header\nrow\nrow...`. `None` when the input
/// isn't such an array or folding wouldn't shrink it.
#[must_use]
pub fn fold(s: &str) -> Option<String> {
    let value: Value = serde_json::from_str(s.trim()).ok()?;
    let arr = value.as_array()?;
    if arr.len() < 2 || !arr.iter().all(Value::is_object) {
        return None;
    }

    // Union of keys, first-seen order, O(n) via a HashSet guard.
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

    let mut out = String::with_capacity(s.len() / 2);
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

    // Pure transform: produce the table for any valid array-of-objects. The
    // shrink economics (is it actually smaller, worth the header) live in the
    // pipeline (`transforms::run` / `compress`), consistent with columnar::fold.
    Some(out)
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

/// Write CSV-escaped, comma-joined fields directly into `out` — no intermediate
/// `Vec` or per-field `String`.
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

/// RFC4180-style escaping: quote iff the field contains comma, quote, CR or LF;
/// internal quotes are doubled. Written straight into `out`.
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
    fn folds_array_of_objects_and_shrinks() {
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
    fn rejects_non_array() {
        assert!(fold(r#"{"a":1}"#).is_none());
        assert!(fold("not json at all").is_none());
        assert!(fold(r#"[1,2,3]"#).is_none());
    }

    #[test]
    fn rejects_single_element_array() {
        assert!(fold(r#"[{"a":1}]"#).is_none());
    }
}
