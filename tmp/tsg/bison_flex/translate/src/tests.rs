//! End-to-end tests: a `.g` source in, a `.tsg` grammar out.
//!
//! Each test is a whole file, because the interesting behaviour only shows up
//! when the four sections are read together: a token body comes from the Flex
//! rules, and a rule's precedence from the declarations.

use crate::{
    emit::{translate as emit, Options},
    ir::Grammar,
};

/// A `.g` file: `%start`, declarations, Bison rules, an empty Flex-definitions
/// section, and Flex rules — the layout `postgres16.g` uses.
fn source(declarations: &str, rules: &str, lexer: &str) -> String {
    with_definitions(declarations, rules, "", lexer)
}

fn with_definitions(declarations: &str, rules: &str, definitions: &str, lexer: &str) -> String {
    format!("%start s\n{declarations}\n%%\n{rules}\n%%\n{definitions}\n%%\n{lexer}\n%%\n")
}

fn read(source: &str) -> Result<Grammar, crate::cst::InputError> {
    crate::cst::read(source.as_bytes(), &crate::language())
}

fn translate(source: &str) -> String {
    let grammar = read(source).unwrap_or_else(|error| panic!("{error}"));
    let options = Options {
        name: "test",
        title: "",
        source: "test.g",
        header: &[],
        conflicts: &[],
        bodies: &[],
    };
    emit(&grammar, &options).expect("translated").text
}

/// Everything after the comment that introduces the start rule.
fn rules(source: &str) -> String {
    let text = translate(source);
    let start = text
        .find("// The start rule is written first.")
        .expect("the rules section marker");
    text[start..].to_string()
}

#[test]
fn a_token_body_is_the_flex_pattern() {
    let text = translate(&source("%token A", "s : A 'x' ;", "A    A"));
    assert!(text.contains("tokens {\n  A: /A/;\n}"), "{text}");
    assert!(text.contains("s:\n    A 'x';"), "{text}");
}

#[test]
fn a_named_flex_pattern_is_expanded_into_its_token() {
    // `{ident}  IDENT` — the macro is substituted, parenthesized so that a
    // surrounding quantifier cannot change its meaning.
    let text = translate(&with_definitions(
        "%token IDENT",
        "s : IDENT ;",
        "ident  [A-Za-z_][A-Za-z_0-9]*",
        "{ident}  IDENT",
    ));
    assert!(
        text.contains("tokens {\n  IDENT: /([A-Za-z_][A-Za-z_0-9]*)/;\n}"),
        "{text}"
    );
}

#[test]
fn option_caseless_reaches_every_token() {
    // `%option caseless` sits in the definitions section, but it applies to
    // the rules in the next section.
    let text = translate(&with_definitions(
        "%token A",
        "s : A ;",
        "%option caseless",
        "A    A",
    ));
    assert!(text.contains("A: /A/i;"), "{text}");
}

#[test]
fn a_nullable_rule_reads_as_an_optional() {
    // An empty production is part of the language. In EBNF that is `Y?` — no
    // empty alternative anywhere — and the rule used once becomes a field, so
    // the use site says both what it is and that it may be absent.
    let text = rules(&source(
        "%token X Y",
        "s : a X ;\na : %empty | Y ;",
        "X X\nY Y",
    ));
    assert!(text.contains("s:\n    a= Y? X;"), "{text}");
    assert!(!text.contains("a:"), "{text}");
    assert!(!text.contains("()"), "{text}");
}

#[test]
fn a_self_recursive_list_is_one_repetition() {
    // `l : l ',' X | %empty` — `l` is "zero or more items", so it is written
    // that way: one rule, no empty alternative, and the repetition cannot loop
    // because each item consumes a token.
    let text = rules(&source(
        "%token X",
        "s : l ;\nl : l ',' X | %empty ;",
        "X X",
    ));
    assert!(text.contains("s:\n    (',' X)*;"), "{text}");
    assert!(!text.contains("l:"), "the name is used once, so it buys nothing");
}

#[test]
fn a_list_of_something_that_starts_empty_is_still_one_repetition() {
    // The item begins with a nullable rule, which used to be refused by the
    // generator ("Unresolved conflict … `tag_opt`"): `expand_repeats` put a
    // binary merge next to the items, and reducing that raced the zero-width
    // reduce of `tag_opt`. Fixed in the generator, so this is one rule too.
    let text = rules(&source(
        "%token X T",
        "s : r ;\nr : r tag_opt X | %empty ;\ntag_opt : %empty | T ;",
        "X X\nT T",
    ));
    // `tag_opt` is used once, so it becomes a field inside the repetition and
    // the item no longer *starts* with a named rule — only with a `?`.
    assert!(text.contains("s:\n    (tag= T? X)*;"), "{text}");
}

#[test]
fn a_nullable_rule_that_is_not_a_list_is_an_optional_too() {
    // `e: %empty | E` used in the middle of a body: it says "an E or nothing",
    // so it is written `E?` — at the one place it is used.
    let text = rules(&source(
        "%token X E",
        "s : e X ;\ne : %empty | E ;",
        "X X\nE E",
    ));
    assert!(text.contains("s:\n    e= E? X;"), "{text}");
    assert!(!text.contains("e:"), "{text}");
}

#[test]
fn a_run_of_nullable_symbols_stays_one_alternative() {
    // `t: a b;` with both nullable is exactly that; the nullable rules supply
    // their own empty productions, so `t` needs no subsequence expansion.
    let text = rules(&source(
        "%token X Y Z",
        "s : t X ;\nt : a b ;\na : %empty | X ;\nb : %empty | Y | Z ;",
        "X X\nY Y\nZ Z",
    ));
    assert!(text.contains("t:\n    a= X? b= (Y | Z)?;"), "{text}");
    assert!(!text.contains("t:\n    a b?\n  | b;"), "{text}");
}

#[test]
fn precedence_levels_are_reversed_and_prec_names_one() {
    let text = translate(&source(
        "%token X\n%left '+'\n%right '*'",
        "s : s '+' s | s '*' s | X %prec '+' ;",
        "X X",
    ));
    // Bison lists levels lowest first; tsg wants a descending chain, and the
    // associativity in `@left(…)` comes from the `%prec` symbol's level.
    assert!(text.contains("%precedence right \"p2\" = '*';"), "{text}");
    assert!(text.contains("%precedence left \"p1\" = '+';"), "{text}");
    assert!(text.contains("  | X @left(p1);"), "{text}");
}

#[test]
fn a_fallback_token_is_offered_after_its_specific_tokens() {
    // `%fallback Y X` says: where the parser cannot use `X`, the scanner gives
    // `Y`. tree-sitter's lexer only produces tokens the state accepts, so the
    // whole translation is the order in the `tokens` block: `X` must come
    // first, or it loses the lexer tie to `Y`.
    let text = translate(&source(
        "%token Y\n%token X\n%fallback Y X",
        "s : Y X ;",
        "[x]+ X\n[y]+ Y",
    ));
    assert!(!text.contains("%fallback"), "{text}");
    let x = text.find("  X:").expect("X has a body");
    let y = text.find("  Y:").expect("Y has a body");
    assert!(x < y, "X is the specific token and has to be offered first\n{text}");
}

#[test]
fn a_declared_start_symbol_becomes_a_start_directive() {
    // Bison's `%start` is a declaration, not a position; the `.tsg` says it
    // rather than relying on which rule the emitter writes first.
    let text = translate(
        "%token A\n%start second\n%%\nfirst : A A ;\nsecond : A ;\n%%\n%%\nA A\n%%\n",
    );
    assert!(text.contains("%start second;"), "{text}");
    assert!(
        text.find("%start second;").unwrap() < text.find("second:").unwrap(),
        "{text}"
    );
}

#[test]
fn a_start_symbol_bison_did_not_name_is_left_positional() {
    // No `%start` in the source: the first rule is the symbol, and writing an
    // explicit directive would claim the file said something it did not.
    let text = translate("%token A\n%%\nfirst : A A ;\nsecond : A ;\n%%\n%%\nA A\n%%\n");
    assert!(!text.contains("%start"), "{text}");
    assert!(text.contains("// Bison named no start symbol"), "{text}");
}

#[test]
fn a_lookahead_token_becomes_its_base_token() {
    // `X_LA` is lexed by a start-condition block that only looks ahead, so the
    // token in the grammar is `X` and the lookahead rule disappears.
    let text = translate(&source("%token X X_LA", "s : X_LA X ;", "X X"));
    assert!(!text.contains("X_LA"), "{text}");
    assert!(text.contains("s:\n    X X;"), "{text}");
}

#[test]
fn an_unsupported_construct_is_rejected_with_its_position() {
    let error = read(&source("%token X", "s : [ X ] ;", "X X")).unwrap_err();
    // The bracket is on line 4, column 5; `row`/`column` are tree-sitter's,
    // `Display` makes them 1-based.
    assert_eq!((error.row, error.column), (3, 4), "{error}");
    assert!(error.to_string().starts_with("4:5: "), "{error}");
    assert!(error.message.contains("groups"), "{}", error.message);
}

#[test]
fn a_syntax_error_points_at_what_did_not_fit() {
    // `%%` inside a rule body: the parser knows a rule ends with `;`, says so,
    // and points at the separator rather than at the end of the file.
    let error = read(&source("%token X", "s : X ;\nt : X %% X ;", "X X")).unwrap_err();
    assert_eq!(error.to_string(), "5:7: expected `;`");
}

#[test]
fn a_name_used_in_a_rule_body_is_a_terminal_even_if_untyped() {
    // `%nonassoc lazy` declares a terminal without a `%token` line, and the
    // lexer section supplies its pattern; bison reads the name in a rule body
    // as a token, so the translation has to as well.
    let text = translate(&source("%nonassoc lazy", "s : lazy X ;", "X X\nlazy lazy"));
    assert!(text.contains("lazy: /lazy/;"), "{text}");
    assert!(text.contains("s:\n    lazy X;"), "{text}");
}

#[test]
fn a_string_literal_terminal_is_not_a_token_rule() {
    // `%token "abc"` declares a literal terminal; tree-sitter reads the
    // literal from the rule body, so it must not become `"abc": /abc/;`.
    let text = translate(&source("%token \"abc\" X", "s : \"abc\" X ;", "X X"));
    assert!(!text.contains("\"abc\":"), "{text}");
    assert!(text.contains("s:\n    \"abc\" X;"), "{text}");
}

#[test]
fn a_slash_in_a_flex_pattern_stays_escaped() {
    // `/` closes the literal in the emitted `.tsg`, inside a character class
    // included, so a Flex `\/` has to survive both rewrites.
    let text = translate(&source("%token RE", "s : RE ;", "\\/[^\\n]*\\/    RE"));
    assert!(text.contains("RE: /\\/[^\\n]*\\//;"), "{text}");
}

#[test]
fn a_production_alias_is_dropped() {
    // `#Name` labels an alternative for other tools; it has no tree-sitter
    // equivalent and does not describe the language, so it is dropped.
    let text = rules(&source("%token X", "s : X #Label ;", "X X"));
    assert!(text.contains("s:\n    X;"), "{text}");
    assert!(!text.contains("#Label"), "{text}");
}

#[test]
fn a_flex_hex_escape_becomes_one_regex_byte() {
    // Flex reads any number of hex digits after `\x`, so `\x0000` is NUL; the
    // regex dialect wants two (`\x00`) and refuses a longer spelling.
    let text = translate(&source(
        "%token STR",
        "s : STR ;",
        r#"[\x0000-\x001F]    STR"#,
    ));
    assert!(text.contains(r"STR: /[\x00-\x1f]/;"), "{text}");
}

#[test]
fn a_rule_whose_productions_all_needed_an_unlexable_token_does_not_become_empty() {
    // `%token LOST` has no lexer rule, so `b` loses its only production. `b`
    // is then unproductive, not nullable: a `b?` here would say the language
    // changed (and the `?` is what made the antlr-v4.5 grammar ambiguous).
    let text = rules(&source(
        "%token A\n%token LOST",
        "s : c ;\na : b ;\nb : LOST ;\nc : A ;",
        "A  A",
    ));
    assert!(text.contains("s:\n    A;"), "{text}");
    assert!(!text.contains("b?"), "{text}");
    assert!(!text.contains("\n    a"), "{text}");
}

#[test]
fn a_start_rule_that_loses_every_production_is_an_error() {
    // Silently promoting the next rule to start symbol would change the
    // language without saying so.
    let grammar = read(&source(
        "%token A\n%token LOST",
        "s : LOST ;\nt : A ;",
        "A  A",
    ))
    .unwrap();
    let options = Options {
        name: "test",
        title: "",
        source: "test.g",
        header: &[],
        conflicts: &[],
        bodies: &[],
    };
    let error = match emit(&grammar, &options) {
        Ok(_) => panic!("translated a grammar with no start rule"),
        Err(error) => error,
    };
    assert!(error.contains("every production of `s`"), "{error}");
}

#[test]
fn a_trailing_comment_after_a_lexer_action_is_not_the_action() {
    let text = translate(&source(
        "%token A\n%token IDENT",
        "s : IDENT ;",
        "[a-z]+    IDENT //FIXME\n",
    ));
    assert!(text.contains("IDENT: /[a-z]+/;"), "{text}");
}

#[test]
fn a_commented_out_lexer_rule_does_not_give_a_token_its_body() {
    let text = translate(&source(
        "%token A\n%token IDENT",
        "s : IDENT ;",
        "//[0-9]+    IDENT\n[a-z]+    IDENT\n/*[0-9]+    A*/\n",
    ));
    assert!(text.contains("IDENT: /[a-z]+/;"), "{text}");
}

#[test]
fn a_balanced_region_start_condition_becomes_a_delimited_external() {
    // The one stateful shape tree-sitter *can* run: the depth is local to the
    // region, so a scanner holds it and the parse never sees a state. Recognising
    // the idiom is what turns a reported loss into an expressible token.
    let text = translate(&source(
        "%token IDENT\n%token ACTION_CODE",
        "s : IDENT ACTION_CODE ;",
        "<INITIAL> {\n    [a-z]+    IDENT\n    \"{\"<>CODE>\n}\n\
         <CODE> {\n    \"{\"<>CODE>\n    \"}\"<<> ACTION_CODE\n    (?s:.)<.>\n}\n",
    ));
    assert!(
        text.contains("%external ACTION_CODE = delimited('{', '}');"),
        "{text}"
    );
    // The push half is the scanner's job now, so it must not be a token body and
    // must not be reported as a loss either.
    assert!(!text.contains("ACTION_CODE:"), "{text}");
    assert!(text.contains("IDENT: /[a-z]+/;"), "{text}");
}

#[test]
fn a_start_condition_that_is_not_a_balanced_region_is_reported_as_pushdown() {
    // `<<>` on its own is a pop, so the group is a state machine and nothing
    // short of a hand-written scanner can do it. It has to say *that*, not
    // "lookahead", or the author is told to fix something they cannot fix.
    let grammar = read(&source(
        "%token A",
        "s : A ;",
        "<st> {\n    [0-9]+    A\n    [a-z]+<st><<>\n}\n",
    ))
    .expect("read");
    assert!(
        grammar
            .lexer
            .notes
            .iter()
            .any(|note| note.contains("switch start conditions") && note.contains("state stack")),
        "{:?}",
        grammar.lexer.notes
    );
    assert!(grammar.lexer.pushdown.is_empty(), "{:?}", grammar.lexer.pushdown);
}

#[test]
fn a_start_condition_group_that_includes_initial_is_expanded() {
    // `bison.g` keeps its whole lexer in `<INITIAL,RULES_ST> { … }`. Flex means
    // "these rules run in those states", so a group that includes INITIAL is
    // expressible in a stateless lexer: expand it. Both the token body and the
    // `skip()` extra have to come out of the group.
    let text = translate(&with_definitions(
        "%token IDENT",
        "s : IDENT ;",
        "SPACES\t[ \\t\\n]+",
        "<INITIAL,RULES_ST> {\n    {SPACES} skip()\n    [a-z]+    IDENT\n}\n",
    ));
    assert!(text.contains("IDENT: /[a-z]+/;"), "{text}");
    assert!(text.contains("%extra /([ \\t\\n]+)/;"), "{text}");
}

#[test]
fn a_state_only_group_is_reported_and_a_state_change_is_reported() {
    // A rule that only runs in `xformat_la` needs a lexer state tree-sitter does
    // not have. Dropping it is right; dropping it *silently* is how a token
    // ends up missing and the note blames the source for not returning it.
    let grammar = read(&source(
        "%token A",
        "s : A ;",
        "<xla> {\n    [0-9]+    A\n}\n",
    ))
    .expect("read");
    assert!(
        grammar
            .lexer
            .notes
            .iter()
            .any(|note| note.contains("start condition") && note.contains("[0-9]+")),
        "{:?}",
        grammar.lexer.notes
    );

    let grammar = read(&source("%token A", "s : A ;", "A<xla>    A\n")).expect("read");
    assert!(
        grammar
            .lexer
            .notes
            .iter()
            .any(|note| note.contains("switch to a start condition")),
        "{:?}",
        grammar.lexer.notes
    );
    // The pattern itself is still usable, so the rule is kept.
    assert_eq!(grammar.lexer.rules, vec![("A".to_string(), "A".to_string())]);
}


// ---- What the rewrite does to a rule ---------------------------------------
//
// One policy row per test: the shape the source wrote on the left, the way EBNF
// would say it on the right. These are cheaper than an end-to-end gate and,
// unlike a gate, they say which row broke.

/// The rewritten rules of a grammar whose lexer returns every token it names.
fn policy(declarations: &str, rules: &str, tokens: &[(&str, &str)]) -> String {
    let lexer: Vec<String> = tokens
        .iter()
        .map(|(pattern, token)| format!("{pattern}\t{token}"))
        .collect();
    super::tests::rules(&source(declarations, rules, &lexer.join("\n")))
}

#[test]
fn a_base_and_its_items_collapse_to_an_optional_base_and_a_star() {
    // R1, one base alternative: `() | P | N I` is `P? I*`.
    let text = policy("%token P I", "s : %empty | P | s I ;", &[("P", "P"), ("I", "I")]);
    assert!(text.contains("s:\n    P? I*;"), "{text}");
}

#[test]
fn several_bases_are_a_group_before_the_star() {
    // R1, several: the base is not one atom, so it keeps a group.
    let text = policy("%token A B C", "s : A | B | s C ;", &[("A", "A"), ("B", "B"), ("C", "C")]);
    assert!(text.contains("s:\n    (A | B) C*;"), "{text}");
}

#[test]
fn an_empty_alternative_makes_only_the_base_optional() {
    // bc's `semicolon_list`: `B? (P B?)*`. The `?` is on the base, not on the
    // whole group: the source allows the items without one.
    let text = policy(
        "%token B P",
        "s : %empty | B | s P | s P B ;",
        &[("B", "B"), ("P", "P")],
    );
    assert!(text.contains("s:\n    B? (P B?)*;"), "{text}");
}

#[test]
fn a_branch_that_extends_another_puts_the_rest_under_a_question_mark() {
    // R2: `A | A B` is `A B?`.
    let text = policy("%token A B", "s : A | A B ;", &[("A", "A"), ("B", "B")]);
    assert!(text.contains("s:\n    A B?;"), "{text}");
}

#[test]
fn a_rule_that_only_matches_nothing_is_erased() {
    // R3: a reference to it contributes nothing, and no `()` is left behind.
    let text = policy(
        "%token A B",
        "s : A t B ;\nt : %empty ;\nu : %empty ;",
        &[("A", "A"), ("B", "B")],
    );
    assert!(text.contains("s:\n    A B;"), "{text}");
    assert!(!text.contains("t:"), "{text}");
}

#[test]
fn a_rule_that_names_one_rule_and_is_used_once_is_that_rule() {
    // R4: the name bought nothing, so the reference speaks for itself.
    let text = policy(
        "%token A B",
        "s : A t ;\nt : B ;",
        &[("A", "A"), ("B", "B")],
    );
    assert!(text.contains("s:\n    A B;"), "{text}");
    assert!(!text.contains("t:"), "{text}");
}

#[test]
fn what_every_branch_shares_is_written_once() {
    // Carbon's `paren_expression_base`, the shape that made factoring worth
    // doing: `'(' C ')' | '(' C ',' ')' | '(' ')'` is `'(' (C ','?)? ')'`.
    let text = policy(
        "%token C",
        "s : '(' ')' | '(' C ')' | '(' C ',' ')' ;",
        &[("C", "C"), ("\",\"", "','")],
    );
    assert!(text.contains("s:\n    '(' (C ','?)? ')';"), "{text}");
}

#[test]
fn consecutive_branches_that_start_alike_choose_after_it() {
    let text = policy(
        "%token A B D",
        "s : A B | A D ;",
        &[("A", "A"), ("B", "B"), ("D", "D")],
    );
    assert!(text.contains("s:\n    A (B | D);"), "{text}");
}

#[test]
fn a_branch_that_is_another_with_an_optional_head_puts_the_head_last() {
    // xmc's `modality`: `'-' t | '-' | t` is `'-'? t`.
    let text = policy(
        "%token A",
        "s : '-' t | '-' | t ;\nt : A ;",
        &[("\"-\"", "'-'"), ("A", "A")],
    );
    assert!(text.contains("s:\n    '-'? t;"), "{text}");
}

#[test]
fn a_repetition_whose_item_nests_the_rule_is_not_a_star() {
    // Carbon's `if_expression`. The language allows `(IF e THEN s ELSE)* s`,
    // but that hides the nesting behind an operator that reads as a flat
    // sequence; factoring says `?` instead, which keeps it visible.
    let text = policy(
        "%token K L X Y",
        "s : K s L s | K s | X | Y ;",
        &[("K", "K"), ("L", "L"), ("X", "X"), ("Y", "Y")],
    );
    assert!(text.contains("s:\n    K s (L s)?\n  | X\n  | Y;"), "{text}");
    assert!(!text.contains("*;"), "{text}");
}

#[test]
fn right_recursion_with_several_bases_is_left_alone() {
    // bc's `statement`: `(For … | While …)* statement` is the same language
    // but reads as a prefix over a machine, not as a list.
    let text = policy(
        "%token P Q T",
        "s : P | Q | T s ;",
        &[("P", "P"), ("Q", "Q"), ("T", "T")],
    );
    assert!(text.contains("s:\n    P\n  | Q\n  | T s;"), "{text}");
}

#[test]
fn a_prec_on_an_empty_alternative_stops_the_fold() {
    // bc's `opt_else : %empty %prec IF_WITHOUT_ELSE` is the dangling-else
    // fix. Folding it into a field would move the decision, so the rule keeps
    // its blank production and its annotation.
    let text = policy(
        "%token A B ELSE\n%left ELSE",
        "s : A t ;\nt : %empty %prec ELSE | ELSE B ;",
        &[("A", "A"), ("B", "B"), ("ELSE", "ELSE")],
    );
    assert!(text.contains("t:\n    () @left(p1)\n  | ELSE B;"), "{text}");
}
