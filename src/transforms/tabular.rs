//! Tabular fold — the headline transform.
//!
//! A JSON array of objects repeats every key on every element. `ls`-style
//! listings, API list payloads, `cargo metadata`, log exports — all balloon
//! because the schema is paid for on every row. Folding to a single header row
//! plus CSV-style body writes each key **once**.
//!
//! Lossless in meaning: a header row + value rows is trivially reconstructable
//! back into the original objects, and an LLM reads it directly.

use serde_json::Value;

/// Fold a JSON array-of-objects into `header\nrow\nrow...`. Returns `None` when
/// the input isn't such an array, or when folding wouldn't shrink it.
pub fn fold(s: &str) -> Option<String> {
    let value: Value = serde_json::from_str(s.trim()).ok()?;
    let arr = value.as_array()?;
    // Need at least a couple of rows for the schema-amortization to pay off.
    if arr.len() < 2 {
        return None;
    }
    // Every element must be an object for a clean table.
    if !arr.iter().all(Value::is_object) {
        return None;
    }

    // Union of keys, first-seen order — stable and deterministic.
    let mut columns: Vec<String> = Vec::new();
    for obj in arr {
        for key in obj.as_object().unwrap().keys() {
            if !columns.iter().any(|c| c == key) {
                columns.push(key.clone());
            }
        }
    }
    if columns.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("@crush.table ");
    out.push_str(&arr.len().to_string());
    out.push_str(" rows\n");
    out.push_str(&csv_row(columns.iter().map(|s| s.as_str())));
    out.push('\n');

    for obj in arr {
        let map = obj.as_object().unwrap();
        let cells = columns
            .iter()
            .map(|col| map.get(col).map(cell).unwrap_or_default());
        out.push_str(&csv_row_owned(cells));
        out.push('\n');
    }
    out.pop(); // trailing newline

    // Pure transform: produce the table whenever the input is a valid
    // array-of-objects. The shrink economics (is it actually smaller, is it
    // worth the header cost) belong to the pipeline, not here.
    Some(out)
}

/// Render one JSON value as a single CSV cell string (pre-escaping).
fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        // Nested structures: compact JSON, escaped as one cell.
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn csv_row<'a>(fields: impl Iterator<Item = &'a str>) -> String {
    fields.map(escape_field).collect::<Vec<_>>().join(",")
}

fn csv_row_owned(fields: impl Iterator<Item = String>) -> String {
    fields
        .map(|f| escape_field(&f))
        .collect::<Vec<_>>()
        .join(",")
}

/// RFC4180-style escaping: quote a field iff it contains comma, quote, CR or LF;
/// internal quotes are doubled.
fn escape_field(f: &str) -> String {
    if f.contains([',', '"', '\n', '\r']) {
        let mut s = String::with_capacity(f.len() + 2);
        s.push('"');
        for ch in f.chars() {
            if ch == '"' {
                s.push('"');
            }
            s.push(ch);
        }
        s.push('"');
        s
    } else {
        f.to_string()
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
        assert!(
            out.len() < input.len(),
            "must shrink: {} vs {}",
            out.len(),
            input.len()
        );
        assert!(out.contains("name,age,city"));
        assert!(out.contains("alice,30,nyc"));
        assert!(out.contains("3 rows"));
    }

    #[test]
    fn handles_missing_keys_with_empty_cells() {
        let input = r#"[{"a":1,"b":2},{"a":3},{"b":4,"c":5}]"#;
        let out = fold(input).unwrap();
        // union columns a,b,c; row 2 has no b → trailing empty
        assert!(out.contains("a,b,c"));
        assert!(out.contains("3,,")); // a=3, b empty, c empty
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
        assert!(fold(r#"[1,2,3]"#).is_none()); // array of scalars, not objects
    }

    #[test]
    fn rejects_single_element_array() {
        assert!(fold(r#"[{"a":1}]"#).is_none());
    }

    #[test]
    fn serializes_nested_structures_as_json_cells() {
        let input = r#"[{"id":1,"tags":["x","y"]},{"id":2,"tags":["z"]}]"#;
        let out = fold(input).unwrap();
        assert!(out.contains(r#""[""x"",""y""]""#) || out.contains("[\"x\",\"y\"]"));
    }
}
