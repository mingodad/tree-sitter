//! A minimal JSON reader.
//!
//! `json2tsg` has to read `grammar.json`, so the crate needs a parser for it.
//! This is a straightforward recursive-descent scanner over bytes with the same
//! span-based error reporting as the `.tsg` front-end.

use crate::json::Json;
use crate::lexer::{locate, Error, Res, Span};

pub fn parse(src: &str) -> Res<Json> {
    let mut reader = Reader {
        src,
        bytes: src.as_bytes(),
        pos: 0,
    };
    let value = reader.value()?;
    reader.skip_trivia();
    if reader.pos < reader.bytes.len() {
        return Err(reader.error(reader.pos, "trailing characters after the JSON value"));
    }
    Ok(value)
}

struct Reader<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn error(&self, offset: usize, msg: impl Into<String>) -> Error {
        Error::new(Span::new(offset, offset + 1), msg)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_trivia(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn describe_here(&self) -> String {
        match self.peek() {
            None => "end of input".to_owned(),
            Some(b) => {
                let (line, column, ..) = locate(self.src, self.pos);
                format!("`{}` at line {line}, column {column}", b as char)
            }
        }
    }

    fn value(&mut self) -> Res<Json> {
        self.skip_trivia();
        let start = self.pos;
        match self.peek() {
            None => Err(self.error(start, "expected a JSON value, but found end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error(
                start,
                format!("expected a JSON value, but found {}", self.describe_here()),
            )),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Res<Json> {
        if self.src[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error(self.pos, format!("expected `{word}`")))
        }
    }

    fn number(&mut self) -> Res<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = &self.src[start..self.pos];
        if is_float {
            return Err(Error::new(
                Span::new(start, self.pos),
                format!("`grammar.json` should not contain the float `{text}`"),
            ));
        }
        match text.parse::<i64>() {
            Ok(n) => Ok(Json::Num(n)),
            Err(_) => Err(Error::new(
                Span::new(start, self.pos),
                format!("`{text}` is not a valid integer"),
            )),
        }
    }

    fn string(&mut self) -> Res<String> {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(Error::new(
                    Span::new(start, self.pos),
                    "unterminated JSON string",
                ));
            };
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape_start = self.pos - 1;
                    let Some(e) = self.peek() else {
                        return Err(Error::new(
                            Span::new(start, self.pos),
                            "unterminated escape sequence",
                        ));
                    };
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape(escape_start)?),
                        other => {
                            return Err(self.error(
                                escape_start,
                                format!("unknown escape `\\{}` in a JSON string", other as char),
                            ))
                        }
                    }
                }
                // Multi-byte UTF-8 sequences are copied through verbatim.
                b if b < 0x80 => out.push(b as char),
                _ => {
                    self.pos -= 1;
                    let Some(ch) = self.src[self.pos..].chars().next() else {
                        return Err(self.error(start, "invalid UTF-8 in a JSON string"));
                    };
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn unicode_escape(&mut self, escape_start: usize) -> Res<char> {
        let hi = self.hex4(escape_start)? as u32;
        if !(0xD800..0xDC00).contains(&hi) {
            return char::from_u32(hi)
                .ok_or_else(|| self.error(escape_start, "invalid `\\u` escape sequence"));
        }
        // Surrogate pair: `\uD8xx\uDCxx`.
        if self.peek() == Some(b'\\') && self.bytes.get(self.pos + 1) == Some(&b'u') {
            let save = self.pos;
            self.pos += 2;
            if let Ok(lo) = self.hex4(escape_start) {
                let lo = lo as u32;
                if (0xDC00..0xE000).contains(&lo) {
                    let combined = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    if let Some(ch) = char::from_u32(combined) {
                        return Ok(ch);
                    }
                }
            }
            self.pos = save;
        }
        Err(self.error(escape_start, "lone surrogate in a `\\u` escape sequence"))
    }

    fn hex4(&mut self, escape_start: usize) -> Res<u16> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let Some(b) = self.peek() else {
                return Err(self.error(escape_start, "truncated `\\u` escape sequence"));
            };
            let Some(digit) = (b as char).to_digit(16) else {
                return Err(self.error(
                    self.pos,
                    "`\\u` must be followed by exactly four hex digits",
                ));
            };
            value = value * 16 + digit as u16;
            self.pos += 1;
        }
        Ok(value)
    }

    fn array(&mut self) -> Res<Json> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_trivia();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => {
                    return Err(self.error(
                        self.pos,
                        format!("expected `,` or `]`, but found {}", self.describe_here()),
                    ))
                }
            }
        }
    }

    fn object(&mut self) -> Res<Json> {
        self.pos += 1; // '{'
        let mut entries: Vec<(String, Json)> = Vec::new();
        self.skip_trivia();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(entries));
        }
        loop {
            self.skip_trivia();
            if self.peek() != Some(b'"') {
                return Err(self.error(
                    self.pos,
                    format!("expected a key, but found {}", self.describe_here()),
                ));
            }
            let key_start = self.pos;
            let key = self.string()?;
            if entries.iter().any(|(existing, _)| *existing == key) {
                return Err(Error::new(
                    Span::new(key_start, self.pos),
                    format!("duplicate key `{key}`"),
                ));
            }
            self.skip_trivia();
            if self.peek() != Some(b':') {
                return Err(self.error(
                    self.pos,
                    format!("expected `:`, but found {}", self.describe_here()),
                ));
            }
            self.pos += 1;
            let value = self.value()?;
            entries.push((key, value));
            self.skip_trivia();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(entries));
                }
                _ => {
                    return Err(self.error(
                        self.pos,
                        format!("expected `,` or `}}`, but found {}", self.describe_here()),
                    ))
                }
            }
        }
    }
}

impl Json {
    /// Look up a key in an object; `None` for other kinds or a missing key.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(entries) => entries
                .iter()
                .find(|(existing, _)| existing == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(value) => Some(*value),
            _ => None,
        }
    }
}
