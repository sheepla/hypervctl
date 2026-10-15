//! CSV rendering per RFC 4180: CRLF line endings, quotes only where they are
//! needed, and no byte order mark (design 4.4).

use serde_json::{Map, Value};

/// Renders a serialized list (an array) or record (an object) as CSV. An empty
/// list produces no output at all, not even a header (design 4.4).
pub fn render(value: &Value, no_header: bool) -> String {
    let rows: Vec<Vec<(String, Value)>> = match value {
        Value::Array(items) => items.iter().map(flatten).collect(),
        other => vec![flatten(other)],
    };
    if rows.is_empty() {
        return String::new();
    }

    let mut keys: Vec<String> = Vec::new();
    for row in &rows {
        for (key, _) in row {
            if !keys.iter().any(|seen| seen == key) {
                keys.push(key.clone());
            }
        }
    }

    let mut out = String::new();
    if !no_header {
        let header: Vec<&str> = keys.iter().map(String::as_str).collect();
        push_record(&mut out, &header);
    }
    for row in &rows {
        let record: Vec<String> = keys.iter().map(|key| field(row, key)).collect();
        let fields: Vec<&str> = record.iter().map(String::as_str).collect();
        push_record(&mut out, &fields);
    }
    out
}

/// Flattens one record into `key, value` pairs: objects join their keys with
/// `_`, arrays of objects add an index, and an array of plain values becomes
/// one comma separated value (design 4.4).
fn flatten(value: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    match value {
        Value::Object(map) => flatten_object(map, None, &mut out),
        other => out.push(("value".to_string(), other.clone())),
    }
    out
}

fn flatten_object(map: &Map<String, Value>, prefix: Option<&str>, out: &mut Vec<(String, Value)>) {
    for (key, value) in map {
        let key = match prefix {
            Some(prefix) => format!("{prefix}_{key}"),
            None => key.clone(),
        };
        match value {
            Value::Object(nested) => flatten_object(nested, Some(key.as_str()), out),
            // An empty array has no fields to show, so it adds no key.
            Value::Array(items) if !items.is_empty() && items.iter().all(is_scalar) => {
                let joined = items.iter().map(text).collect::<Vec<_>>().join(", ");
                out.push((key, Value::String(joined)));
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let key = format!("{key}_{index}");
                    match item {
                        Value::Object(nested) => flatten_object(nested, Some(key.as_str()), out),
                        other => out.push((key, other.clone())),
                    }
                }
            }
            scalar => out.push((key, scalar.clone())),
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

/// The text a value contributes to a cell: nothing at all for `null`
/// (design 4.4).
fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn field(row: &[(String, Value)], key: &str) -> String {
    row.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| text(value))
        .unwrap_or_default()
}

fn push_record(out: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&quote(field));
    }
    out.push_str("\r\n");
}

/// Quotes a field only where RFC 4180 asks for it, doubling the quotes inside.
fn quote(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\r') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn records_are_separated_by_crlf_and_quoted_on_demand() {
        let value = json!([
            {"name": "plain", "note": null},
            {"name": "with,comma", "note": "say \"hi\""},
        ]);
        assert_eq!(
            render(&value, false),
            "name,note\r\nplain,\r\n\"with,comma\",\"say \"\"hi\"\"\"\r\n"
        );
    }

    #[test]
    fn the_header_can_be_left_out() {
        let value = json!([{"name": "web-01"}]);
        assert_eq!(render(&value, true), "web-01\r\n");
    }

    #[test]
    fn an_empty_list_produces_no_output() {
        let empty = json!([]);
        assert_eq!(render(&empty, false), "");
        assert_eq!(render(&empty, true), "");
    }

    #[test]
    fn objects_join_their_keys_and_arrays_add_an_index() {
        let value = json!({
            "name": "web-01",
            "memory": {"startup_bytes": 1024, "min_bytes": null},
            "nics": [{"switch_name": "lan", "mac_address": "00:15:5d:01:02:03"}],
            "disks": [],
            "tags": ["a", "b"],
            "empty_tags": [],
        });
        let row = flatten(&value);
        let pairs: Vec<String> = row
            .iter()
            .map(|(key, value)| format!("{key}={}", text(value)))
            .collect();
        // `disks` and `empty_tags` are empty arrays: they carry no fields and
        // therefore no key.
        assert_eq!(
            pairs,
            [
                "name=web-01",
                "memory_startup_bytes=1024",
                "memory_min_bytes=",
                "nics_0_switch_name=lan",
                "nics_0_mac_address=00:15:5d:01:02:03",
                "tags=a, b",
            ]
        );
    }

    #[test]
    fn rows_without_a_key_get_no_column() {
        let value = json!([
            {"disks": [{"path": "one"}]},
            {"disks": []},
        ]);
        assert_eq!(render(&value, false), "disks_0_path\r\none\r\n\r\n");
    }

    #[test]
    fn a_record_renders_as_a_header_and_one_row() {
        let value = json!({"name": "web-01", "state": "running"});
        assert_eq!(render(&value, false), "name,state\r\nweb-01,running\r\n");
    }
}
