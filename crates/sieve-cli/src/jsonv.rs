//! A JSON value read from text that keeps the text's key order.
//!
//! `serde_json::Value` sorts object keys here (no `preserve_order`), and
//! `ojson::OJson` builds values but never parses them. `stats --json` and
//! `telemetry debug` print a file's keys in the order the file holds them,
//! as `JSON.stringify(v, null, 2)` does, so they read through this type.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value with insertion-ordered object keys.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Builds a string value.
    pub fn s(v: impl Into<String>) -> Self {
        Json::Str(v.into())
    }

    /// Builds an integer value.
    pub fn n(v: i64) -> Self {
        Json::Num(serde_json::Number::from(v))
    }

    /// Builds an object from an ordered key list.
    pub fn obj(pairs: Vec<(&str, Json)>) -> Self {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Parses JSON text. `None` when the text is not JSON.
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// Reads a field of an object. `None` for a missing key or a non-object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value as `f64`. `None` for a non-number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => n.as_f64(),
            _ => None,
        }
    }

    /// The value as `&str`. `None` for a non-string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The object's pairs, or an empty slice for a non-object.
    pub fn pairs(&self) -> &[(String, Json)] {
        match self {
            Json::Obj(pairs) => pairs,
            _ => &[],
        }
    }

    /// Sets a key as a JavaScript object assignment does: an existing key
    /// keeps its position and takes the new value; a new key goes last.
    pub fn set(pairs: &mut Vec<(String, Json)>, key: &str, value: Json) {
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => pairs.push((key.to_string(), value)),
        }
    }

    /// Renders the value as `JSON.stringify(v, null, 2)` does, with no
    /// trailing newline.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        let pad = |n: usize| "  ".repeat(n);
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => out.push_str(&n.to_string()),
            Json::Str(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&pad(depth + 1));
                    item.write(out, depth + 1);
                }
                out.push('\n');
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Obj(pairs) if pairs.is_empty() => out.push_str("{}"),
            Json::Obj(pairs) => {
                out.push_str("{\n");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    out.push_str(&pad(depth + 1));
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push_str(": ");
                    v.write(out, depth + 1);
                }
                out.push('\n');
                out.push_str(&pad(depth));
                out.push('}');
            }
        }
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Num(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Num(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Json, E> {
        Ok(serde_json::Number::from_f64(v).map_or(Json::Null, Json::Num))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Json, E> {
        Ok(Json::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Arr(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut pairs: Vec<(String, Json)> = Vec::new();
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            // A repeated key keeps the first position, as `JSON.parse` does.
            Json::set(&mut pairs, &k, v);
        }
        Ok(Json::Obj(pairs))
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p1_64_parse_keeps_key_order_and_prints_like_js() {
        let text = r#"{"z":1,"a":{"y":null,"b":[]},"m":[1,"x",true],"e":{},"f":1.5}"#;
        let v = Json::parse(text).expect("parse");
        assert_eq!(
            v.to_pretty(),
            "{\n  \"z\": 1,\n  \"a\": {\n    \"y\": null,\n    \"b\": []\n  },\n  \"m\": [\n    1,\n    \"x\",\n    true\n  ],\n  \"e\": {},\n  \"f\": 1.5\n}"
        );
        assert_eq!(v.get("z").and_then(Json::as_f64), Some(1.0));
        assert_eq!(Json::parse("{not json"), None);
        assert_eq!(Json::parse("null"), Some(Json::Null));
    }

    #[test]
    fn test_p1_64_set_keeps_position_of_existing_key() {
        let mut pairs = vec![("a".to_string(), Json::n(1)), ("b".to_string(), Json::n(2))];
        Json::set(&mut pairs, "a", Json::n(9));
        Json::set(&mut pairs, "c", Json::n(3));
        assert_eq!(
            Json::Obj(pairs).to_pretty(),
            "{\n  \"a\": 9,\n  \"b\": 2,\n  \"c\": 3\n}"
        );
    }
}
