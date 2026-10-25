; Queries for the .tsg language, shipped next to the grammar the way a real
; parser package does (`queries/highlights.scm`).
;
; There is no `@error` rule for regex flags, and that is a deliberate limit.
; `i` is the only flag with an effect, and the grammar says so by putting the
; flags inside the literal: `/x/i` is one `regex` token, `/x/ i` is a regex
; followed by a symbol. A letter that is not a flag cannot be captured as a bad
; flag, because no rule can make `/x/g` an error — a token pattern cannot see
; whitespace — so it parses as a regex and the symbol `g`. Rejecting it is
; `tsg`'s job, and it does, with the fix in the hint (`put a space after the
; literal: /…/ g`).

(comment) @comment

(string) @string
(regex) @string.regexp

(function_name (identifier) @function)
(rule_definition name: (identifier) @function.definition)
(symbol (identifier) @variable)

(escaped_symbol) @variable.special
(integer) @number

; `%precedence left "add" = '+' '-';` — the associativity is a keyword, the level
; name is a precedence rather than an ordinary string, and the terminals are the
; productions' inheritance keys.
(precedence_directive associativity: _ @keyword)
(precedence_name (string) @string.special)
(precedence_call associativity: _ @keyword)
(precedence_call precedence: (string) @string.special)

; `@left(2)` marks a use, which is why it is an operator and not a call.
(annotation "@" @operator)
