//! A tiny JSON reader. It only has to understand the Bing archive reply, but it
//! follows the JSON grammar, so any valid JSON document parses.
//!
//! Deliberate leniency: raw control characters inside strings are accepted,
//! so one odd character in a caption cannot break the whole sync.

const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Look up a key in an object. With duplicate keys, the last one wins.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
}

/// Parse one JSON document. Trailing text after the document is an error.
pub fn parse(input: &str) -> Result<Value, String> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut p = Parser { s: input.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("invalid JSON at byte {}: {}", self.i, what)
    }

    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("unknown literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        self.ws();
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.eat(b'{')?;
        let mut items = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Obj(items));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            let v = self.value(depth + 1)?;
            items.push((key, v));
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Obj(items));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Arr(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while matches!(self.s.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.i += 1;
        }
        // The slice holds only ASCII bytes, so it is valid UTF-8.
        let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad number"))?;
        text.parse::<f64>().map(Value::Num).map_err(|_| self.err("bad number"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let mut n = 0u32;
        for &b in digits {
            let d = (b as char).to_digit(16).ok_or_else(|| self.err("bad \\u escape"))?;
            n = n * 16 + d;
        }
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            // Copy a run of plain bytes in one go. `"` and `\` are ASCII, so a
            // run always ends on a UTF-8 character boundary.
            let start = self.i;
            while !matches!(self.s.get(self.i), None | Some(b'"' | b'\\')) {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad UTF-8"))?);
            match self.s.get(self.i) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => {
                    self.i += 1; // the backslash
                    let c = *self.s.get(self.i).ok_or_else(|| self.err("unterminated escape"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.err("bad escape")),
                    }
                }
            }
        }
    }

    /// Called after `\u`. Joins a surrogate pair; a lone surrogate becomes U+FFFD.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
            let save = self.i;
            self.i += 2;
            let lo = self.hex4()?;
            if (0xDC00..0xE000).contains(&lo) {
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                return Ok(char::from_u32(cp).unwrap_or('\u{fffd}'));
            }
            self.i = save; // not a pair: the next escape is read on its own
        }
        Ok(char::from_u32(hi).unwrap_or('\u{fffd}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars() {
        assert_eq!(parse("null"), Ok(Value::Null));
        assert_eq!(parse(" true "), Ok(Value::Bool(true)));
        assert_eq!(parse("false"), Ok(Value::Bool(false)));
        assert_eq!(parse("-12.5e1"), Ok(Value::Num(-125.0)));
        assert_eq!(parse("\"hi\""), Ok(Value::Str("hi".into())));
    }

    #[test]
    fn nested_structures() {
        let v = parse(r#"{"a":[1,{"b":null},[]],"c":{},"d":"x"}"#).unwrap();
        assert_eq!(v.get("d").and_then(Value::as_str), Some("x"));
        let a = v.get("a").and_then(Value::as_array).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].get("b"), Some(&Value::Null));
        assert_eq!(v.get("c"), Some(&Value::Obj(vec![])));
        assert_eq!(v.get("missing"), None);
    }

    #[test]
    fn string_escapes() {
        let v = parse(r#""a\"b\\c\/d\n\t\u0041\u00e9""#).unwrap();
        assert_eq!(v.as_str(), Some("a\"b\\c/d\n\tA\u{e9}"));
    }

    #[test]
    fn surrogate_pairs_and_lone_surrogates() {
        assert_eq!(parse(r#""\ud83d\ude00""#).unwrap().as_str(), Some("\u{1F600}"));
        assert_eq!(parse(r#""\ud83dx""#).unwrap().as_str(), Some("\u{fffd}x"));
        assert_eq!(parse(r#""\ude00""#).unwrap().as_str(), Some("\u{fffd}"));
        // A high surrogate followed by an ordinary escape keeps both.
        assert_eq!(parse(r#""\ud83d\u0041""#).unwrap().as_str(), Some("\u{fffd}A"));
    }

    #[test]
    fn raw_utf8_is_kept() {
        let v = parse("{\"t\":\"Cr\u{e8}me br\u{fb}l\u{e9}e \u{2014} \u{65e5}\u{672c}\"}").unwrap();
        assert_eq!(v.get("t").and_then(Value::as_str), Some("Cr\u{e8}me br\u{fb}l\u{e9}e \u{2014} \u{65e5}\u{672c}"));
    }

    #[test]
    fn byte_order_mark_is_skipped() {
        assert_eq!(parse("\u{feff}[1]"), Ok(Value::Arr(vec![Value::Num(1.0)])));
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let v = parse(r#"{"k":"first","k":"second"}"#).unwrap();
        assert_eq!(v.get("k").and_then(Value::as_str), Some("second"));
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "", "{", "[1,", "{\"a\"}", "{\"a\":}", "[1 2]", "\"abc", "\"\\x\"", "\"\\u12\"", "tru", "nul", "01x", "{} x", "[,]",
            "{\"a\":1,}", "-",
        ] {
            assert!(parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_deep_nesting() {
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_err());
        let ok = "[".repeat(20) + &"]".repeat(20);
        assert!(parse(&ok).is_ok());
    }
}
