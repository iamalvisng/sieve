//! A JSON value that remembers the field order it was built in.
//!
//! This workspace's `serde_json` has no `preserve_order` feature, so a
//! `serde_json::Value` object always serializes its keys alphabetically
//! (`Map` is a `BTreeMap`). Several golden files pin a key order that is
//! not alphabetical (`command` before `args`, `statusLine` before
//! `hooks`), so `sieve init` builds those files with this small ordered
//! type instead, and only reaches for `serde_json::Value` to read or
//! compare existing content.

use std::collections::BTreeMap;

/// A JSON value with insertion-ordered object fields.
#[derive(Debug, Clone, PartialEq)]
pub enum OJson {
    Str(String),
    Bool(bool),
    Num(i64),
    /// A number kept as its source text (a float, an exponent), so a
    /// parsed foreign file round-trips it unchanged.
    Raw(String),
    /// A JSON `null`, kept by [`OJson::parse`].
    Null,
    Arr(Vec<OJson>),
    Obj(Vec<(String, OJson)>),
}

impl OJson {
    /// Builds a string value.
    pub fn s(v: impl Into<String>) -> Self {
        OJson::Str(v.into())
    }

    /// Builds an object from an ordered list of key-value pairs.
    pub fn obj(pairs: Vec<(&str, OJson)>) -> Self {
        OJson::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Builds an array value.
    pub fn arr(items: Vec<OJson>) -> Self {
        OJson::Arr(items)
    }

    /// Renders this value as 2-space-indented JSON text, matching
    /// `JSON.stringify(v, null, 2)`, with no trailing newline.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            OJson::Str(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
            OJson::Num(n) => out.push_str(&n.to_string()),
            OJson::Raw(r) => out.push_str(r),
            OJson::Null => out.push_str("null"),
            OJson::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            OJson::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    item.write(out, indent + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(indent));
                out.push(']');
            }
            OJson::Obj(pairs) => {
                if pairs.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push_str(": ");
                    v.write(out, indent + 1);
                    if i + 1 < pairs.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(indent));
                out.push('}');
            }
        }
    }

    /// Parses JSON text into an [`OJson`] that keeps every object's key
    /// order and every number and `null` as written. Returns `None` for
    /// text that is not one valid JSON value. JS `JSON.parse` and
    /// `JSON.stringify` keep key order, so a strip rewrite must too.
    pub fn parse(text: &str) -> Option<Self> {
        // The strict serde parse decides validity; this parse only keeps order.
        serde_json::from_str::<serde_json::Value>(text).ok()?;
        let mut p = Parser {
            b: text.as_bytes(),
            text,
            at: 0,
        };
        p.value()
    }

    /// Returns this value as an object's ordered pairs, or `None`.
    pub fn as_obj(&self) -> Option<&[(String, OJson)]> {
        match self {
            OJson::Obj(pairs) => Some(pairs),
            _ => None,
        }
    }

    /// Returns this value as an array's items, or `None`.
    pub fn as_arr(&self) -> Option<&[OJson]> {
        match self {
            OJson::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// Returns this value as a string, or `None`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            OJson::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Looks up one key on an object value, or `None` when this value is
    /// not an object or the key is absent.
    pub fn get(&self, key: &str) -> Option<&OJson> {
        self.as_obj()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// A tiny insertion-ordered map over [`OJson`] object fields, used while a
/// merge builds up a new top-level object one key at a time.
pub struct OMap {
    order: Vec<String>,
    values: BTreeMap<String, OJson>,
}

impl OMap {
    /// Starts an empty ordered map.
    pub fn new() -> Self {
        OMap {
            order: Vec::new(),
            values: BTreeMap::new(),
        }
    }

    /// Seeds this map from an existing object's fields, in that object's
    /// (already alphabetical) order.
    pub fn from_existing(existing: Option<&OJson>) -> Self {
        let mut map = OMap::new();
        if let Some(pairs) = existing.and_then(OJson::as_obj) {
            for (k, v) in pairs {
                map.set(k, v.clone());
            }
        }
        map
    }

    /// Sets a key's value, keeping its first-seen position, or appending
    /// it when it is new.
    pub fn set(&mut self, key: &str, value: OJson) {
        if !self.values.contains_key(key) {
            self.order.push(key.to_string());
        }
        self.values.insert(key.to_string(), value);
    }

    /// Removes a key, if present.
    pub fn remove(&mut self, key: &str) {
        if self.values.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }

    /// Returns `true` when this map has no keys left.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Renders this map's keys in their tracked order.
    pub fn into_ojson(self) -> OJson {
        let pairs = self
            .order
            .into_iter()
            .map(|k| {
                let v = self.values.get(&k).cloned().unwrap_or(OJson::s(""));
                (k, v)
            })
            .collect();
        OJson::Obj(pairs)
    }
}

/// Prints a number the way `JSON.stringify` does (ECMAScript
/// `Number::toString`): `1.50` is `1.5`, `1.5e3` is `1500`, `1e21` is
/// `1e+21`, `1e-7` is `1e-7`. A value that overflows to infinity prints
/// `null`, as `JSON.stringify(Infinity)` does.
fn js_number(raw: &str) -> String {
    let Ok(f) = raw.parse::<f64>() else {
        return raw.to_string();
    };
    if !f.is_finite() {
        return "null".to_string();
    }
    if f == 0.0 {
        return "0".to_string();
    }
    // Rust's `{:e}` gives the shortest round-trip digits: `d.ddde<exp>`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let e = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{e}")
        } else {
            format!("{}.{}e{sign}{e}", &digits[..1], &digits[1..])
        }
    };
    if f < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

/// A small recursive-descent reader over text that serde already
/// validated. It exists only to keep object key order.
struct Parser<'a> {
    b: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.at) == Some(&c)).then(|| self.at += 1)
    }

    fn string(&mut self) -> Option<String> {
        let start = self.at;
        self.at += 1;
        while *self.b.get(self.at)? != b'"' {
            self.at += if self.b[self.at] == b'\\' { 2 } else { 1 };
        }
        self.at += 1;
        serde_json::from_str(&self.text[start..self.at]).ok()
    }

    fn value(&mut self) -> Option<OJson> {
        self.ws();
        match *self.b.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut pairs: Vec<(String, OJson)> = Vec::new();
                while self.eat(b'}').is_none() {
                    self.eat(b',');
                    self.ws();
                    let key = self.string()?;
                    self.eat(b':')?;
                    let v = self.value()?;
                    // `JSON.parse`: a repeated key keeps its first place, last value.
                    match pairs.iter_mut().find(|(k, _)| *k == key) {
                        Some(slot) => slot.1 = v,
                        None => pairs.push((key, v)),
                    }
                }
                Some(OJson::Obj(pairs))
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                while self.eat(b']').is_none() {
                    self.eat(b',');
                    items.push(self.value()?);
                }
                Some(OJson::Arr(items))
            }
            b'"' => self.string().map(OJson::Str),
            b't' => self.word("true", OJson::Bool(true)),
            b'f' => self.word("false", OJson::Bool(false)),
            b'n' => self.word("null", OJson::Null),
            _ => {
                let start = self.at;
                while self
                    .b
                    .get(self.at)
                    .is_some_and(|c| c.is_ascii_digit() || b"+-.eE".contains(c))
                {
                    self.at += 1;
                }
                let raw = &self.text[start..self.at];
                Some(
                    raw.parse::<i64>()
                        .map_or_else(|_| OJson::Raw(js_number(raw)), OJson::Num),
                )
            }
        }
    }

    fn word(&mut self, w: &str, v: OJson) -> Option<OJson> {
        self.at += w.len();
        Some(v)
    }
}

impl Default for OMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_pretty_matches_json_stringify_two_space_indent() {
        let v = OJson::obj(vec![
            ("command", OJson::s("sieve")),
            ("args", OJson::arr(vec![OJson::s("mcp")])),
        ]);
        assert_eq!(
            v.to_pretty(),
            "{\n  \"command\": \"sieve\",\n  \"args\": [\n    \"mcp\"\n  ]\n}"
        );
    }

    /// P4-49: expected values are node's `JSON.stringify(JSON.parse(raw))`
    /// on v24.18.0.
    #[test]
    fn test_p4_49_js_number_matches_node() {
        let cases = [
            ("1.50", "1.5"),
            ("1.5e3", "1500"),
            ("1e-7", "1e-7"),
            ("1e21", "1e+21"),
            ("1e20", "100000000000000000000"),
            ("123456789012345678901", "123456789012345680000"),
            ("1.2345e-7", "1.2345e-7"),
            ("0.000001", "0.000001"),
            ("0.0000001", "1e-7"),
            ("-1.5e-9", "-1.5e-9"),
            ("1e400", "null"),
            ("-0.0", "0"),
            ("0.1", "0.1"),
            ("100.25", "100.25"),
            ("5e-324", "5e-324"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"),
            ("2.5e22", "2.5e+22"),
            ("123e-2", "1.23"),
            ("0.5E+2", "50"),
            ("12345678901234567890123.5", "1.2345678901234568e+22"),
        ];
        for (raw, want) in cases {
            assert_eq!(js_number(raw), want, "raw {raw}");
        }
    }

    #[test]
    fn omap_keeps_insertion_order_and_drops_removed_keys() {
        let mut m = OMap::new();
        m.set("b", OJson::s("2"));
        m.set("a", OJson::s("1"));
        m.remove("b");
        m.set("b", OJson::s("2-again"));
        let OJson::Obj(pairs) = m.into_ojson() else {
            panic!("expected an object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "b"]);
    }
}
