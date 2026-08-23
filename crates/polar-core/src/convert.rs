//! ProseMirror JSON attribute values <-> yrs `Any`.

use polar_schema::Attrs as PmAttrs;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use yrs::Any;

pub fn json_to_any(value: &Value) -> Any {
    match value {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        // Integers round-trip exactly as BigInt; f64 would quietly widen them
        // and turn `level: 1` into `level: 1.0` on the way back.
        Value::Number(n) => match n.as_i64() {
            Some(i) => Any::BigInt(i),
            None => Any::Number(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Any::String(s.as_str().into()),
        Value::Array(items) => Any::Array(items.iter().map(json_to_any).collect()),
        Value::Object(map) => Any::Map(Arc::new(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_any(v)))
                .collect(),
        )),
    }
}

pub fn any_to_json(any: &Any) -> Value {
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Any::BigInt(i) => Value::Number((*i).into()),
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(bytes) => Value::Array(
            bytes
                .iter()
                .map(|b| Value::Number((*b as i64).into()))
                .collect(),
        ),
        Any::Array(items) => Value::Array(items.iter().map(any_to_json).collect()),
        Any::Map(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), any_to_json(v)))
                .collect(),
        ),
    }
}

pub fn any_map_to_attrs(map: &HashMap<String, Any>) -> PmAttrs {
    map.iter()
        .map(|(k, v)| (k.clone(), any_to_json(v)))
        .collect()
}
