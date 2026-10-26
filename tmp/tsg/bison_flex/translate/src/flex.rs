//! Flex patterns → tree-sitter regexes.
//!
//! Two textual rewrites, both of which need to know what is inside a character
//! class or a quoted string: macro expansion (`{name}`) and escape translation.
//!
//! The one thing the target dialect adds is `/`: it closes the literal in the
//! emitted `.tsg`, so every `/` in a pattern — in a character class too — has to
//! come out escaped as `\/`.

use crate::ir::Lexer;

/// Substitute `{macro}` references, wrapping each substitution in parentheses
/// so an expanded macro cannot change the precedence of its context.
pub fn expand(lexer: &Lexer, pattern: &str, depth: usize) -> Result<String, String> {
    if depth > 20 {
        return Err(format!("macro recursion in `{pattern}`"));
    }
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            // A lone `{` is a repetition count in Flex, not a macro.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        if !is_bare_word(name) {
            // `{1,3}` and friends: keep Flex's repetition syntax.
            out.push_str(&rest[start..start + end + 2]);
            rest = &after[end + 1..];
            continue;
        }
        let Some((_, body)) = lexer
            .macros
            .iter()
            .find(|(macro_name, _)| macro_name == name)
        else {
            return Err(format!("unknown macro `{{{name}}}` in `{pattern}`"));
        };
        out.push('(');
        out.push_str(&expand(lexer, body, depth + 1)?);
        out.push(')');
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Flex pattern → the regex dialect tree-sitter accepts.
///
/// Handles the constructs a real `.l` file uses: `\c` escapes for regex
/// metacharacters, octal bytes (`\200`–`\377`, which Flex uses for the
/// high-bit bytes), double-quoted literal strings, and character classes,
/// which are copied through because Flex and tree-sitter agree on their syntax.
pub fn to_regex(pattern: &str) -> Result<String, String> {
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(pattern.len());
    let mut index = 0;
    let mut in_class = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' && index + 1 < bytes.len() {
            let next = bytes[index + 1];
            if next.is_ascii_digit() && next < b'8' {
                let mut end = index + 1;
                while end < bytes.len() && end < index + 4 && (b'0'..=b'7').contains(&bytes[end]) {
                    end += 1;
                }
                let digits = &pattern[index + 1..end];
                let value = u32::from_str_radix(digits, 8)
                    .map_err(|_| format!("bad octal escape `\\{digits}` in `{pattern}`"))?;
                out.push_str(&byte_escape(value));
                index = end;
                continue;
            }
            // Flex reads as many hex digits as are there, so `\x0000` is one
            // byte; the regex dialect wants exactly two (`\x00`) below 0x80.
            if next == b'x' {
                let mut end = index + 2;
                while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                    end += 1;
                }
                if end > index + 2 {
                    let digits = &pattern[index + 2..end];
                    let value = u32::from_str_radix(digits, 16)
                        .map_err(|_| format!("bad hex escape `\\x{digits}` in `{pattern}`"))?;
                    out.push_str(&byte_escape(value));
                    index = end;
                    continue;
                }
            }
            match next {
                b'"' => out.push('"'),
                // Flex spells the control characters C's way; the regex dialect
                // wants a byte, and `\b` is *not* a word boundary here (it is
                // backspace, and assertions are unsupported anyway).
                b'a' => out.push_str("\\x07"),
                b'b' => out.push_str("\\x08"),
                b'e' => out.push_str("\\x1b"),
                b'f' => out.push_str("\\x0c"),
                b'v' => out.push_str("\\x0b"),
                b'n' | b'r' | b't' => {
                    out.push('\\');
                    out.push(char::from(next));
                }
                // The shorthand classes Flex shares with the regex dialect.
                b's' | b'S' | b'w' | b'W' | b'd' | b'D' => {
                    out.push('\\');
                    out.push(char::from(next));
                }
                // Any other letter is an escaped literal in Flex.
                b if b.is_ascii_alphabetic() => out.push(char::from(b)),
                b'.' | b'[' | b']' | b'{' | b'}' | b'(' | b')' | b'*' | b'+' | b'?' | b'|'
                | b'^' | b'$' | b'\\' | b'/' => {
                    out.push('\\');
                    out.push(char::from(next));
                }
                _ => out.push(char::from(next)),
            }
            index += 2;
            continue;
        }
        if byte == b'"' && !in_class {
            // The closing quote is the first one that is not escaped; Flex
            // writes a literal quote inside a string as `\"`.
            let body_start = index + 1;
            let mut end = body_start;
            let mut body = String::new();
            loop {
                if end >= bytes.len() {
                    return Err(format!("unterminated string in `{pattern}`"));
                }
                match bytes[end] {
                    b'\\' if end + 1 < bytes.len() => {
                        body.push(unescaped(bytes[end + 1]));
                        end += 2;
                    }
                    b'"' => {
                        end += 1;
                        break;
                    }
                    other => {
                        body.push(char::from(other));
                        end += 1;
                    }
                }
            }
            out.push_str(&escape_literal(&body));
            index = end;
            continue;
        }
        // Flex patterns cannot contain a bare `/*`: a comment after the
        // pattern (or, in the corpus, right before the action) is not part of
        // it.
        if byte == b'/' && !in_class && bytes.get(index + 1) == Some(&b'*') {
            match pattern[index..].find("*/") {
                Some(len) => {
                    index += len + 2;
                    continue;
                }
                None => return Err(format!("unterminated comment in `{pattern}`")),
            }
        }
        // `/` closes the literal in the emitted `.tsg`, so it always needs the
        // escape, inside a character class included.
        if byte == b'/' {
            out.push_str("\\/");
            index += 1;
            continue;
        }
        if byte == b'[' && !in_class {
            in_class = true;
        } else if byte == b']' && in_class {
            in_class = false;
        }
        out.push(char::from(byte));
        index += 1;
    }
    Ok(out)
}

/// A byte or code point, spelled the way the regex dialect wants it.
fn byte_escape(value: u32) -> String {
    if value <= 0x7f {
        format!("\\x{value:02x}")
    } else {
        format!("\\x{{{value:x}}}")
    }
}

/// What a Flex escape inside a string stands for: the character itself, with
/// the C spellings decoded because a regex has no `\n` escape for a literal
/// newline.
fn unescaped(byte: u8) -> char {
    match byte {
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'a' => '\u{7}',
        b'b' => '\u{8}',
        b'e' => '\u{1b}',
        b'f' => '\u{c}',
        b'v' => '\u{b}',
        other => char::from(other),
    }
}

/// The characters Flex puts inside `"…"`, escaped the way Python's
/// `re.escape` escapes them, so a keyword body reads `/ABORT/i`.
pub fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        // A literal in the emitted `.tsg` is delimited by `/`, and a control
        // character has to be a regex escape: `/a\nb/` cannot be written with
        // a real newline in it.
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\x0b"),
            '\u{c}' => out.push_str("\\x0c"),
            '(' | ')' | '[' | ']' | '{' | '}' | '?' | '*' | '+' | '-' | '|' | '^' | '$' | '\\'
            | '.' | '&' | '~' | '#' | ' ' | '/' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// `[A-Za-z_][A-Za-z0-9_]*`: a bare word, the shape a keyword rule has.
pub fn is_bare_word(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}
