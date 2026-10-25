//! Printing string literals back into `.tsg`.
//!
//! Both printers — `--fmt`/`--annotate` (`emit`) and `--from-json` (`to_tsg`)
//! — reach the same values, and a value is not its source text: the parser has
//! already turned `'\0'`, `'\x41'` and a line break inside quotes into the
//! bytes they denote. Writing the value back verbatim therefore writes *those
//! bytes*: `'\0'` becomes a raw NUL, and a string containing a newline splits
//! the line it is on.
//!
//! That is not a grammar difference — the value is the same either way — which
//! is why it survived for so long: every comparison that looked at
//! `grammar.json` agreed. What it breaks is the *file*: a NUL or a stray line
//! break makes the output unusable to anything that reads the text back,
//! including this project's own self-description (`selfhost.sh` failed on a
//! `'\0'` in a corpus grammar for exactly this reason).
//!
//! So there is one escaper, and both printers use it.

/// A string literal, picking the quote style that needs the fewest escapes.
pub fn quote(value: &str) -> String {
    let has_single = value.contains('\'');
    let has_double = value.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };

    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// A `/regex/flags` literal.
///
/// Two things have to be escaped or the literal does not come back: a `/`
/// outside a character class (it would close the literal early — unless the
/// value already escaped it) and any raw control byte (a regex is one line;
/// a newline in the value is `\n` in the literal, which means the same
/// thing to the pattern engine).
pub fn regex_literal(value: &str, flags: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('/');
    let mut in_class = false;
    let mut escaped = false;
    for c in value.chars() {
        match c {
            '\\' if !escaped => {
                escaped = true;
                out.push(c);
                continue;
            }
            '[' if !escaped => {
                in_class = true;
                out.push(c);
            }
            ']' if !escaped => {
                in_class = false;
                out.push(c);
            }
            '/' if !escaped && !in_class => out.push_str("\\/"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            _ => out.push(c),
        }
        escaped = false;
    }
    out.push('/');
    out.push_str(flags);
    out
}

#[cfg(test)]
mod tests {
    use super::{quote, regex_literal};

    #[test]
    fn a_value_is_written_back_as_an_escape_not_as_its_bytes() {
        assert_eq!(quote("\0"), r"'\0'");
        assert_eq!(quote("\x07"), r"'\x07'");
        assert_eq!(quote("\x7f"), r"'\x7f'");
        assert_eq!(quote("a\nb"), r"'a\nb'");
        assert_eq!(quote("a\tb"), r"'a\tb'");
        assert_eq!(quote("\\"), r"'\\'");
        assert_eq!(quote("it's"), r#""it's""#);
        assert_eq!(quote("say \"hi\""), r#"'say "hi"'"#);
        assert_eq!(quote("'"), r#""'""#);
        assert_eq!(quote("' and \""), r#"'\' and "'"#);
    }

    #[test]
    fn a_regex_is_written_on_one_line_and_cannot_close_early() {
        assert_eq!(regex_literal("a/b", ""), r"/a\/b/");
        assert_eq!(regex_literal("a\\/b", ""), r"/a\/b/", "already escaped");
        assert_eq!(
            regex_literal("[/]", "i"),
            "/[/]/i",
            "inside a class a slash closes nothing"
        );
        assert_eq!(regex_literal("[x\\]/y]", ""), r"/[x\]/y]/");
        assert_eq!(regex_literal("a\nb", ""), r"/a\x0ab/");
        assert_eq!(regex_literal("\0", ""), r"/\x00/");
        assert_eq!(regex_literal("\\s", ""), r"/\s/");
    }

    #[test]
    fn no_output_contains_a_control_byte() {
        for value in ["\0", "\x01\x02", "a\nb", "\x7f", "\u{1f600}"] {
            let printed = quote(value);
            assert!(
                !printed.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f),
                "{value:?} printed as {printed:?}"
            );
        }
    }
}