use serde_json::Value;

// Helpers for reading loosely-typed tool arguments coming from the model.

/// Parses the JSON blob a model sent as function-call arguments.
///
/// Degrades to `{}` rather than throwing: the model is free to send `""`,
/// truncated JSON or a bare array, and the tool's own readers then supply their
/// fallbacks.
pub fn parse_tool_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => as_record(value),
        Err(_) => Value::Object(serde_json::Map::new()),
    }
}

/// Narrows an untyped value to a plain object. Arrays and scalars become `{}`.
pub fn as_record(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map),
        _ => Value::Object(serde_json::Map::new()),
    }
}

pub fn read_string(args: &Value, key: &str, fallback: &str) -> String {
    match args.get(key).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => fallback.to_string(),
    }
}

pub fn read_optional_string(args: &Value, key: &str) -> Option<String> {
    let value = args.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn read_number(args: &Value, key: &str, fallback: f64) -> f64 {
    match args.get(key).and_then(Value::as_f64) {
        Some(value) if value.is_finite() && value > 0.0 => value,
        _ => fallback,
    }
}

pub fn read_optional_number(args: &Value, key: &str) -> Option<i64> {
    args.get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value.trunc() as i64)
}

pub fn clamp(value: f64, min: i64, max: i64) -> i64 {
    let truncated = value.trunc() as i64;
    truncated.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unparsable_arguments_degrade_to_an_empty_object() {
        for raw in ["", "   ", "not json", "[1,2]", "\"scalar\""] {
            assert_eq!(parse_tool_arguments(raw), serde_json::json!({}));
        }
    }

    #[test]
    fn readers_trim_and_fall_back() {
        let args = parse_tool_arguments(r#"{"prompt":"  一只红隼  ","n":"12","neg":-3}"#);
        assert_eq!(read_string(&args, "prompt", "默认"), "一只红隼");
        assert_eq!(read_string(&args, "missing", "默认"), "默认");
        assert_eq!(read_optional_string(&args, "blank"), None);
        assert_eq!(read_number(&args, "n", 1.0), 1.0, "a JSON string is not a number");
        assert_eq!(read_number(&args, "neg", 4.0), 4.0, "non-positive falls back");
        assert_eq!(read_optional_number(&args, "neg"), Some(-3));
        assert_eq!(clamp(12.9, 1, 10), 10);
    }
}
