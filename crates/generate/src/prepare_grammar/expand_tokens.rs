use regex_syntax::hir::{Class, Hir, HirKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    grammars::{LexicalGrammar, LexicalVariable},
    nfa::{CharacterSet, Nfa, NfaState},
    prepare_grammar::{
        LexicalToken,
        pattern::{self, RegexError},
    },
    rules::{Precedence, Rule, RuleId, RulePool, Symbol},
};

/// The states that `expand_rule` created for one token's body: the `Accept` state, the body
/// states that follow it, and whether the token is `@immediate` (in which case separators
/// cannot precede it).
struct Range {
    accept: u32,
    body_start: u32,
    is_immediate: bool,
}

struct NfaBuilder {
    nfa: Nfa,
    is_sep: bool,
    precedence_stack: Vec<i32>,
}

pub type ExpandTokensResult<T> = Result<T, ExpandTokensError>;

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExpandTokensError {
    #[error(
        "The rule `{0}` matches the empty string.
Tree-sitter does not support syntactic rules that match the empty string
unless they are used only as the grammar's start rule.
"
    )]
    EmptyString(Box<str>),
    #[error(transparent)]
    Processing(ExpandTokensProcessingError),
    #[error(transparent)]
    ExpandRule(ExpandRuleError),
}

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpandTokensProcessingError {
    rule: Box<str>,
    error: ExpandRuleError,
}

impl std::fmt::Display for ExpandTokensProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Error processing rule {}: {}", self.rule, self.error)
    }
}

fn get_implicit_precedence(pool: &RulePool, root: RuleId) -> i32 {
    let mut id = root;
    let mut boost = 0;
    loop {
        match pool.node(id) {
            Rule::String(_) => return 2 + boost,
            Rule::Metadata { params, rule } => {
                if pool.params(params).is_main_token {
                    boost += 1;
                }
                id = rule;
            }
            _ => return boost,
        }
    }
}

fn get_completion_precedence(pool: &RulePool, id: RuleId) -> i32 {
    if let Rule::Metadata { params, .. } = pool.node(id)
        && let Precedence::Integer(p) = pool.params(params).precedence
    {
        return p;
    }
    0
}

pub fn expand_tokens(
    pool: &mut RulePool,
    lexical_variables: &[LexicalToken],
    separator_roots: &[RuleId],
) -> ExpandTokensResult<LexicalGrammar> {
    let mut builder = NfaBuilder {
        nfa: Nfa::new(),
        is_sep: true,
        precedence_stack: vec![0],
    };
    let separator_root = build_separator(pool, separator_roots);

    let mut ranges = Vec::with_capacity(lexical_variables.len());

    let mut variables = Vec::with_capacity(lexical_variables.len());
    for (i, variable) in lexical_variables.iter().enumerate() {
        if pool.subtree_matches_empty_str(variable.root) {
            Err(ExpandTokensError::EmptyString(
                pool.resolve(variable.name).to_string().into(),
            ))?;
        }
        let is_immediate_token = match pool.node(variable.root) {
            Rule::Metadata { params, .. } => pool.params(params).is_main_token,
            _ => false,
        };

        builder.is_sep = false;
        let accept = builder.nfa.states.len() as u32;
        builder.nfa.states.push(NfaState::Accept {
            variable_index: i,
            precedence: get_completion_precedence(pool, variable.root),
        });
        let last_state_id = builder.nfa.last_state_id();
        builder
            .expand_rule(pool, variable.root, last_state_id)
            .map_err(|e| {
                ExpandTokensError::Processing(ExpandTokensProcessingError {
                    rule: pool.resolve(variable.name).to_string().into(),
                    error: e,
                })
            })?;

        let body_start = builder.nfa.last_state_id();
        ranges.push(Range {
            accept,
            body_start,
            is_immediate: is_immediate_token,
        });

        if !is_immediate_token {
            builder.is_sep = true;
            let last_state_id = builder.nfa.last_state_id();
            builder
                .expand_rule(pool, separator_root, last_state_id)
                .map_err(ExpandTokensError::ExpandRule)?;
        }

        variables.push(LexicalVariable {
            name: variable.name,
            kind: variable.kind,
            implicit_precedence: get_implicit_precedence(pool, variable.root),
            start_state: builder.nfa.last_state_id(),
        });
    }

    let (shared_nfa, shared_owner, shared_start) = build_shared_nfa(
        &builder.nfa,
        &ranges,
        &variables,
        !separator_roots.is_empty(),
    );

    Ok(LexicalGrammar {
        nfa: builder.nfa,
        variables,
        shared_nfa,
        shared_owner,
        shared_start,
    })
}

/// Build an NFA that is equivalent to `nfa`, but where the separator rules are expanded only
/// once: every token body is a private subgraph, and the single shared copy of the separator
/// rules exits into a hub that splits over every (non-immediate) token body.
///
/// `sep* a | sep* b` is the same language as `sep* (a | b)`, so filtering the state sets to
/// the tokens valid in a parse state gives exactly the same lex table as the per-token
/// copies in `nfa`, but with state sets that are proportional to the number of *live*
/// tokens instead of the total number of tokens.
fn build_shared_nfa(
    nfa: &Nfa,
    ranges: &[Range],
    variables: &[LexicalVariable],
    has_separators: bool,
) -> (Nfa, Vec<u32>, Vec<u32>) {
    /// The owner of a state that belongs to the shared separator block or the hub.
    const SHARED: u32 = u32::MAX;

    let mut shared = Nfa::default();
    let mut owner = Vec::<u32>::with_capacity(nfa.states.len());
    let mut body_start = Vec::with_capacity(ranges.len());
    let mut start = Vec::with_capacity(ranges.len());

    // Copy each token's private body, renumbering its states to be contiguous.
    for (i, range) in ranges.iter().enumerate() {
        let new_accept = shared.states.len() as u32;
        shared
            .states
            .push(nfa.states[range.accept as usize].clone());
        owner.push(i as u32);
        for id in (range.accept + 1)..=range.body_start {
            let remap = |target: u32| target - range.accept + new_accept;
            shared
                .states
                .push(nfa.states[id as usize].clone_with_targets(remap));
            owner.push(i as u32);
        }
        body_start.push(new_accept + (range.body_start - range.accept));
        start.push(body_start[i]);
    }

    if has_separators && let Some(first) = ranges.iter().position(|r| !r.is_immediate) {
        // A binary split tree over every non-immediate token body: the exit of the shared
        // separator block. Its leaves are the bodies themselves; the lex table builder drops
        // the ones that belong to tokens which are not valid in the current parse state.
        let mut level = (0..ranges.len())
            .filter(|&i| !ranges[i].is_immediate)
            .map(|i| body_start[i])
            .collect::<Vec<_>>();
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            for pair in level.chunks(2) {
                let id = shared.states.len() as u32;
                let right = pair.get(1).copied().unwrap_or(pair[0]);
                shared.states.push(NfaState::Split(pair[0], right));
                owner.push(SHARED);
                next.push(id);
            }
            level = next;
        }
        let hub = level[0];

        // Every non-immediate token's copy of the separators has the same shape, so one
        // shared copy can serve them all.
        debug_assert!(ranges.iter().enumerate().all(|(i, r)| {
            r.is_immediate
                || variables[i].start_state.wrapping_sub(r.body_start + 1)
                    == variables[first]
                        .start_state
                        .wrapping_sub(ranges[first].body_start + 1)
        }));

        // Copy the separator block once, redirecting its exits to the hub.
        let block_start = ranges[first].body_start + 1;
        let block_end = variables[first].start_state;
        let new_block_start = shared.states.len() as u32;
        for id in block_start..=block_end {
            let remap = |target: u32| {
                if (block_start..=block_end).contains(&target) {
                    target - block_start + new_block_start
                } else {
                    hub
                }
            };
            shared
                .states
                .push(nfa.states[id as usize].clone_with_targets(remap));
            owner.push(SHARED);
        }
        let separator_start = new_block_start + (block_end - block_start);

        for (i, range) in ranges.iter().enumerate() {
            if !range.is_immediate {
                start[i] = separator_start;
            }
        }
    }

    (shared, owner, start)
}

fn build_separator(pool: &mut RulePool, separator_roots: &[RuleId]) -> RuleId {
    let blank = pool.push_node(Rule::Blank);
    if separator_roots.is_empty() {
        return blank;
    }
    let mut elements = Vec::with_capacity(separator_roots.len() + 1);
    let mut stack = Vec::with_capacity(separator_roots.len() + 1);
    stack.push(blank);
    stack.extend(separator_roots.iter().rev().copied());
    while let Some(id) = stack.pop() {
        if let Rule::Choice(range) = pool.node(id) {
            let base = stack.len();
            stack.extend_from_slice(pool.child_slice(range));
            stack[base..].reverse();
        } else if !elements.iter().any(|&e| pool.subtree_eq(e, id)) {
            elements.push(id);
        }
    }
    let range = pool.push_children(&elements);
    let choice = pool.push_node(Rule::Choice(range));
    pool.push_node(Rule::Repeat(choice))
}

pub type ExpandRuleResult<T> = Result<T, ExpandRuleError>;

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExpandRuleError {
    #[error("unexpected symbol {0:?}")]
    UnexpectedSymbol(Symbol),
    #[error("unexpected reserved-word context {0}")]
    UnexpectedReserved(Box<str>),
    #[error(
        "`eof()` cannot be used inside a token. \
        A lexical rule cannot check for end of input, \
        so use `eof()` only at the end of a syntactic rule."
    )]
    UnexpectedEof,
    #[error(transparent)]
    Parse(#[from] Box<RegexError>),
    #[error(transparent)]
    ExpandRegex(ExpandRegexError),
}

pub type ExpandRegexResult<T> = Result<T, ExpandRegexError>;

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExpandRegexError {
    #[error("{0}")]
    Utf8(Box<str>),
    #[error("Regex error: Assertions are not supported")]
    Assertion,
    #[error(transparent)]
    NonAsciiByteClass(NonAsciiByteClassError),
}

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub struct NonAsciiByteClassError {
    start: u8,
    end: u8,
}

impl std::fmt::Display for NonAsciiByteClassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { start, end } = *self;
        write!(f, "The non-ASCII byte class ")?;
        if start == end {
            write!(f, "\\x{start:02x}")?;
        } else {
            write!(f, "\\x{start:02x}-\\x{end:02x}")?;
        }
        write!(f, " via (?-u:...) is not supported. Remove the `-u` flag.")
    }
}

impl NfaBuilder {
    fn expand_rule(
        &mut self,
        pool: &RulePool,
        id: RuleId,
        mut next_state_id: u32,
    ) -> ExpandRuleResult<bool> {
        match pool.node(id) {
            Rule::Pattern(s, f) => {
                // With unicode enabled, `\w`, `\s` and `\d` expand to character sets that are much
                // larger than intended, so we replace them with the actual
                // character sets they should represent. If the full unicode range
                // of `\w`, `\s` or `\d` are needed then `\p{L}`, `\p{Z}` and `\p{N}` should be
                // used.
                let s = pool
                    .resolve(s)
                    .replace(r"\w", r"[0-9A-Za-z_]")
                    .replace(r"\s", r"[\t-\r ]")
                    .replace(r"\d", r"[0-9]")
                    .replace(r"\W", r"[^0-9A-Za-z_]")
                    .replace(r"\S", r"[^\t-\r ]")
                    .replace(r"\D", r"[^0-9]");
                // Parse WITHOUT case folding and fold ourselves (see
                // `case_fold_ascii_safe`). Letting `regex_syntax` fold would pull the
                // long s `ſ` and Kelvin sign `K` into ASCII `s`/`k`.
                let hir = pattern::parse(&s, pool.resolve(f).contains('i'))?;
                self.expand_regex(&hir, next_state_id)
                    .map_err(ExpandRuleError::ExpandRegex)
            }
            Rule::String(s) => {
                let s = pool.resolve(s);
                for c in s.chars().rev() {
                    self.push_advance(CharacterSet::from_char(c), next_state_id);
                    next_state_id = self.nfa.last_state_id();
                }
                Ok(!s.is_empty())
            }
            Rule::Choice(range) => {
                let elements = pool.child_slice(range);
                let mut alternative_state_ids = Vec::with_capacity(elements.len());
                let mut did_expand = false;
                for &element in elements {
                    if self.expand_rule(pool, element, next_state_id)? {
                        did_expand = true;
                        alternative_state_ids.push(self.nfa.last_state_id());
                    } else {
                        alternative_state_ids.push(next_state_id);
                    }
                }
                alternative_state_ids.sort_unstable();
                alternative_state_ids.dedup();
                alternative_state_ids.retain(|i| *i != self.nfa.last_state_id());
                for alternative_state_id in alternative_state_ids {
                    self.push_split(alternative_state_id);
                }
                Ok(did_expand)
            }
            Rule::Seq(range) => {
                let mut result = false;
                for &element in pool.child_slice(range).iter().rev() {
                    if self.expand_rule(pool, element, next_state_id)? {
                        result = true;
                    }
                    next_state_id = self.nfa.last_state_id();
                }
                Ok(result)
            }
            Rule::Repeat(rule) => {
                self.nfa.states.push(NfaState::Accept {
                    variable_index: 0,
                    precedence: 0,
                }); // Placeholder for split
                let split_state_id = self.nfa.last_state_id();
                if self.expand_rule(pool, rule, split_state_id)? {
                    self.nfa.states[split_state_id as usize] =
                        NfaState::Split(self.nfa.last_state_id(), next_state_id);
                    Ok(true)
                } else {
                    self.nfa.states.pop();
                    Ok(false)
                }
            }
            Rule::Metadata { params, rule } => {
                let has_precedence =
                    if let Precedence::Integer(precedence) = pool.params(params).precedence {
                        self.precedence_stack.push(precedence);
                        true
                    } else {
                        false
                    };
                let result = self.expand_rule(pool, rule, next_state_id);
                if has_precedence {
                    self.precedence_stack.pop();
                }
                result
            }
            Rule::Blank => Ok(false),
            Rule::Eof => Err(ExpandRuleError::UnexpectedEof)?,
            Rule::Sym(symbol) => Err(ExpandRuleError::UnexpectedSymbol(symbol))?,
            Rule::Reserved { ctx, .. } => Err(ExpandRuleError::UnexpectedReserved(
                pool.resolve(ctx).to_string().into(),
            ))?,
            // `NamedSymbol` is interned to `Sym` by intern_symbols
            Rule::NamedSymbol(_) => unreachable!(),
        }
    }

    fn expand_regex(&mut self, hir: &Hir, mut next_state_id: u32) -> ExpandRegexResult<bool> {
        match hir.kind() {
            HirKind::Empty => Ok(false),
            HirKind::Literal(literal) => {
                for character in std::str::from_utf8(&literal.0)
                    .map_err(|e| ExpandRegexError::Utf8(e.to_string().into()))?
                    .chars()
                    .rev()
                {
                    self.push_advance(CharacterSet::from_char(character), next_state_id);
                    next_state_id = self.nfa.last_state_id();
                }

                Ok(true)
            }
            HirKind::Class(class) => match class {
                Class::Unicode(class) => {
                    let mut chars = CharacterSet::default();
                    for c in class.ranges() {
                        chars = chars.add_range(c.start(), c.end());
                    }
                    self.push_advance(chars, next_state_id);
                    Ok(true)
                }
                // Byte classes only come from non-Unicode `(?-u:...)` groups, which
                // JS regex syntax can't express. A byte below `0x80` is its own
                // code point, so it converts exactly. Above that, a byte is one
                // part of a UTF-8 sequence and has no character to convert to.
                Class::Bytes(bytes_class) => {
                    let mut chars = CharacterSet::default();
                    for c in bytes_class.ranges() {
                        if !c.end().is_ascii() {
                            Err(ExpandRegexError::NonAsciiByteClass(
                                NonAsciiByteClassError {
                                    start: c.start(),
                                    end: c.end(),
                                },
                            ))?;
                        }
                        chars = chars.add_range(c.start().into(), c.end().into());
                    }
                    self.push_advance(chars, next_state_id);
                    Ok(true)
                }
            },
            HirKind::Look(_) => Err(ExpandRegexError::Assertion)?,
            HirKind::Repetition(repetition) => match (repetition.min, repetition.max) {
                (0, Some(1)) => self.expand_zero_or_one(&repetition.sub, next_state_id),
                (1, None) => self.expand_one_or_more(&repetition.sub, next_state_id),
                (0, None) => self.expand_zero_or_more(&repetition.sub, next_state_id),
                (min, Some(max)) if min == max => {
                    self.expand_count(&repetition.sub, min, next_state_id)
                }
                (min, None) => {
                    if self.expand_zero_or_more(&repetition.sub, next_state_id)? {
                        self.expand_count(&repetition.sub, min, next_state_id)
                    } else {
                        Ok(false)
                    }
                }
                (min, Some(max)) => {
                    let mut result = self.expand_count(&repetition.sub, min, next_state_id)?;
                    for _ in min..max {
                        if result {
                            next_state_id = self.nfa.last_state_id();
                        }
                        if self.expand_zero_or_one(&repetition.sub, next_state_id)? {
                            result = true;
                        }
                    }
                    Ok(result)
                }
            },
            HirKind::Capture(capture) => self.expand_regex(&capture.sub, next_state_id),
            HirKind::Concat(concat) => {
                let mut result = false;
                for hir in concat.iter().rev() {
                    if self.expand_regex(hir, next_state_id)? {
                        result = true;
                        next_state_id = self.nfa.last_state_id();
                    }
                }
                Ok(result)
            }
            HirKind::Alternation(alternations) => {
                let mut alternative_state_ids = Vec::with_capacity(alternations.len());
                for hir in alternations {
                    if self.expand_regex(hir, next_state_id)? {
                        alternative_state_ids.push(self.nfa.last_state_id());
                    } else {
                        alternative_state_ids.push(next_state_id);
                    }
                }
                alternative_state_ids.sort_unstable();
                alternative_state_ids.dedup();
                alternative_state_ids.retain(|i| *i != self.nfa.last_state_id());
                for alternative_state_id in alternative_state_ids {
                    self.push_split(alternative_state_id);
                }
                Ok(true)
            }
        }
    }

    fn expand_one_or_more(&mut self, hir: &Hir, next_state_id: u32) -> ExpandRegexResult<bool> {
        self.nfa.states.push(NfaState::Accept {
            variable_index: 0,
            precedence: 0,
        }); // Placeholder for split
        let split_state_id = self.nfa.last_state_id();
        if self.expand_regex(hir, split_state_id)? {
            self.nfa.states[split_state_id as usize] =
                NfaState::Split(self.nfa.last_state_id(), next_state_id);
            Ok(true)
        } else {
            self.nfa.states.pop();
            Ok(false)
        }
    }

    fn expand_zero_or_one(&mut self, hir: &Hir, next_state_id: u32) -> ExpandRegexResult<bool> {
        if self.expand_regex(hir, next_state_id)? {
            self.push_split(next_state_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expand_zero_or_more(&mut self, hir: &Hir, next_state_id: u32) -> ExpandRegexResult<bool> {
        if self.expand_one_or_more(hir, next_state_id)? {
            self.push_split(next_state_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expand_count(
        &mut self,
        hir: &Hir,
        count: u32,
        mut next_state_id: u32,
    ) -> ExpandRegexResult<bool> {
        let mut result = false;
        for _ in 0..count {
            if self.expand_regex(hir, next_state_id)? {
                result = true;
                next_state_id = self.nfa.last_state_id();
            }
        }
        Ok(result)
    }

    fn push_advance(&mut self, chars: CharacterSet, state_id: u32) {
        let precedence = *self.precedence_stack.last().unwrap();
        self.nfa.states.push(NfaState::Advance {
            chars,
            state_id,
            precedence,
            is_sep: self.is_sep,
        });
    }

    fn push_split(&mut self, state_id: u32) {
        let last_state_id = self.nfa.last_state_id();
        self.nfa
            .states
            .push(NfaState::Split(state_id, last_state_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        grammars::VariableType,
        nfa::{NfaCursor, NfaTransition},
    };

    fn retain_allowed(cursor: &mut NfaCursor<'_>, allowed: Option<&[bool]>) {
        if let Some(allowed) = allowed {
            cursor
                .state_ids
                .retain(|&state_id| allowed[state_id as usize]);
        }
    }

    fn simulate_nfa<'a>(grammar: &'a LexicalGrammar, s: &'a str) -> Option<(usize, &'a str)> {
        let start_states = grammar.variables.iter().map(|v| v.start_state).collect();
        simulate(&grammar.nfa, start_states, s, None)
    }

    /// The same simulation over the `shared_nfa`, with the state set restricted to `tokens`
    /// and the shared separator block, the way the lex table builder does it. Returns the
    /// index of the token that matched instead of the id of its accept state.
    fn simulate_shared<'a>(
        grammar: &'a LexicalGrammar,
        tokens: &[usize],
        s: &'a str,
    ) -> Option<(usize, &'a str)> {
        let allowed = grammar
            .shared_owner
            .iter()
            .map(|&owner| owner == u32::MAX || tokens.contains(&(owner as usize)))
            .collect::<Vec<_>>();
        simulate(
            &grammar.shared_nfa,
            grammar.shared_start.clone(),
            s,
            Some(&allowed),
        )
    }

    fn simulate<'a>(
        nfa: &'a Nfa,
        start_states: Vec<u32>,
        s: &'a str,
        allowed: Option<&[bool]>,
    ) -> Option<(usize, &'a str)> {
        let mut cursor = NfaCursor::new(nfa, start_states);
        retain_allowed(&mut cursor, allowed);

        let mut result = None;
        let mut result_precedence = i32::MIN;
        let mut start_char = 0;
        let mut end_char = 0;
        for c in s.chars() {
            for (id, precedence) in cursor.completions() {
                if result.is_none() || result_precedence <= precedence {
                    result = Some((id, &s[start_char..end_char]));
                    result_precedence = precedence;
                }
            }
            if let Some(NfaTransition {
                states,
                is_separator,
                ..
            }) = cursor
                .transitions()
                .into_iter()
                .find(|t| t.characters.contains(c) && t.precedence >= result_precedence)
            {
                cursor.reset(states);
                retain_allowed(&mut cursor, allowed);
                end_char += c.len_utf8();
                if is_separator {
                    start_char = end_char;
                }
            } else {
                break;
            }
        }

        for (id, precedence) in cursor.completions() {
            if result.is_none() || result_precedence <= precedence {
                result = Some((id, &s[start_char..end_char]));
                result_precedence = precedence;
            }
        }

        result
    }

    fn check(
        //                                   (token roots, separator roots)
        build: impl FnOnce(&mut RulePool) -> (Vec<RuleId>, Vec<RuleId>),
        examples: &[(&str, Option<(usize, &str)>)],
    ) {
        let mut pool = RulePool::default();
        let (roots, separators) = build(&mut pool);
        let vars = roots
            .into_iter()
            .enumerate()
            .map(|(i, root)| LexicalToken {
                name: pool.intern(&format!("tok{i}")),
                kind: VariableType::Anonymous,
                root,
            })
            .collect::<Vec<_>>();
        let grammar = expand_tokens(&mut pool, &vars, &separators).unwrap();
        for &(input, expected) in examples {
            assert_eq!(simulate_nfa(&grammar, input), expected, "input {input}");

            // The shared NFA, restricted to the token that won above, must match the same
            // text; with every token allowed it must still reject a non-match.
            match expected {
                Some((index, text)) => assert_eq!(
                    simulate_shared(&grammar, &[index], input),
                    Some((index, text)),
                    "shared_nfa input {input}"
                ),
                None => {
                    let all = (0..grammar.variables.len()).collect::<Vec<_>>();
                    assert_eq!(
                        simulate_shared(&grammar, &all, input),
                        None,
                        "shared_nfa input {input}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_rule_expansion() {
        // regex with sequences and alternatives
        check(
            |p| {
                let (v, f) = (p.intern("(a|b|c)d(e|f|g)h?"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("ade1", Some((0, "ade"))),
                ("bdf1", Some((0, "bdf"))),
                ("bdfh1", Some((0, "bdfh"))),
                ("ad1", None),
            ],
        );
        // regex with repeats
        check(
            |p| {
                let (v, f) = (p.intern("a*"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("aaa1", Some((0, "aaa"))), ("b", Some((0, "")))],
        );
        // regex with repeats in sequences
        check(
            |p| {
                let (v, f) = (p.intern("a((bc)+|(de)*)f"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("af1", Some((0, "af"))),
                ("adedef1", Some((0, "adedef"))),
                ("abcbcbcf1", Some((0, "abcbcbcf"))),
                ("a", None),
            ],
        );
        // regex with character ranges
        check(
            |p| {
                let (v, f) = (p.intern("[a-fA-F0-9]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("A1ff0.", Some((0, "A1ff0")))],
        );
        // regex with perl character classes
        check(
            |p| {
                let (v, f) = (p.intern("\\w\\d\\s"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("_0  ", Some((0, "_0 ")))],
        );
        // string
        check(
            |p| {
                let s = p.intern("abc");
                (vec![p.string(s)], vec![])
            },
            &[("abcd", Some((0, "abc"))), ("ab", None)],
        );
        // complex rule containing strings and regexes
        check(
            |p| {
                let (reg, empty) = (p.intern("[a-f]+"), p.intern(""));
                let (lb, pat, rb) = (p.intern("{"), p.pattern(reg, empty), p.intern("}"));
                let (left, right) = (p.string(lb), p.string(rb));
                let seq = p.seq(&[left, pat, right]);
                (vec![p.repeat(seq)], vec![])
            },
            &[
                ("{a}{", Some((0, "{a}"))),
                ("{a}{d", Some((0, "{a}"))),
                ("ab", None),
            ],
        );
        // longest match rule
        check(
            |p| {
                let empty = p.intern("");
                let (x, y, z) = (p.intern("a|bc"), p.intern("aa"), p.intern("bcd"));
                (
                    vec![
                        p.pattern(x, empty),
                        p.pattern(y, empty),
                        p.pattern(z, empty),
                    ],
                    vec![],
                )
            },
            &[
                ("a.", Some((0, "a"))),
                ("bc.", Some((0, "bc"))),
                ("aa.", Some((1, "aa"))),
                ("bcd?", Some((2, "bcd"))),
                ("b.", None),
                ("c.", None),
            ],
        );
        // regex with an alternative including the empty string
        check(
            |p| {
                let (v, f) = (p.intern("a(b|)+c"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("ac.", Some((0, "ac"))),
                ("abc.", Some((0, "abc"))),
                ("abbc.", Some((0, "abbc"))),
            ],
        );
        // separators
        check(
            |p| {
                let (v, f) = (p.intern("[a-f]+"), p.intern(""));
                let token = p.pattern(v, f);
                let escaped_newline = {
                    let value = p.intern("\\\n");
                    p.string(value)
                };
                let whitespace = {
                    let value = p.intern("\\s");
                    p.pattern(value, f)
                };
                (vec![token], vec![escaped_newline, whitespace])
            },
            &[
                ("  a", Some((0, "a"))),
                ("  \nb", Some((0, "b"))),
                ("  \\a", None),
                ("  \\\na", Some((0, "a"))),
            ],
        );
        // shorter tokens with higher precedence
        check(
            |p| {
                let f = p.intern("");
                let (v1, v2, v3) = (p.intern("abc"), p.intern("ab[cd]e"), p.intern("[a-e]+"));
                let (pat1, pat2, pat3) = (p.pattern(v1, f), p.pattern(v2, f), p.pattern(v3, f));
                (
                    vec![
                        p.prec(Precedence::Integer(2), pat1),
                        p.prec(Precedence::Integer(1), pat2),
                        pat3,
                    ],
                    vec![],
                )
            },
            &[
                ("abceef", Some((0, "abc"))),
                ("abdeef", Some((1, "abde"))),
                ("aeeeef", Some((2, "aeeee"))),
            ],
        );
        // immediate tokens with higher precedence
        check(
            |p| {
                let f = p.intern("");
                let (v1, v2) = (p.intern("[^a]+"), p.intern("[^ab]+"));
                let (pat1, pat2) = (p.pattern(v1, f), p.pattern(v2, f));
                let r1 = p.prec(Precedence::Integer(1), pat1);
                let r2 = {
                    let imm = p.prec(Precedence::Integer(2), pat2);
                    p.immediate_token(imm)
                };
                let sep = {
                    let s = p.intern("\\s");
                    p.pattern(s, f)
                };
                (vec![r1, r2], vec![sep])
            },
            &[("cccb", Some((1, "ccc")))],
        );
        check(
            |p| {
                let (a, b, c, d) = (p.intern("a"), p.intern("b"), p.intern("c"), p.intern("d"));
                let r1 = p.string(a);
                let r2 = {
                    let (inner_1, inner_2) = (p.string(b), p.string(c));
                    p.choice(&[inner_1, inner_2])
                };
                let r3 = p.string(d);
                (vec![p.seq(&[r1, r2, r3])], vec![])
            },
            &[
                ("abd", Some((0, "abd"))),
                ("acd", Some((0, "acd"))),
                ("abc", None),
                ("ad", None),
                ("d", None),
                ("a", None),
            ],
        );
        // nested choices within sequences
        check(
            |p| {
                let r1 = {
                    let (v, f) = (p.intern("[0-9]+"), p.intern(""));
                    p.pattern(v, f)
                };
                let r2 = {
                    let blank = p.blank();
                    let inner_ch = {
                        let ch1 = {
                            let (e1, e2) = (p.intern("e"), p.intern("E"));
                            let (e1, e2) = (p.string(e1), p.string(e2));
                            p.choice(&[e1, e2])
                        };
                        let ch2 = {
                            let blank = p.blank();
                            let (plus, minus) = (p.intern("+"), p.intern("-"));
                            let (plus, minus) = (p.string(plus), p.string(minus));
                            let inner_ch = p.choice(&[plus, minus]);
                            p.choice(&[blank, inner_ch])
                        };
                        let pat = {
                            let (v, f) = (p.intern("[0-9]+"), p.intern(""));
                            p.pattern(v, f)
                        };
                        let sq = p.seq(&[ch1, ch2, pat]);
                        p.choice(&[sq])
                    };
                    p.choice(&[blank, inner_ch])
                };
                let seq = p.seq(&[r1, r2]);
                (vec![seq], vec![])
            },
            &[
                ("12", Some((0, "12"))),
                ("12e", Some((0, "12"))),
                ("12g", Some((0, "12"))),
                ("12e3", Some((0, "12e3"))),
                ("12e+", Some((0, "12"))),
                ("12E+34 +", Some((0, "12E+34"))),
                ("12e34", Some((0, "12e34"))),
            ],
        );
        // nested groups
        check(
            |p| {
                let (v, f) = (p.intern(r"([^x\\]|\\(.|\n))+"), p.intern(""));
                let pat = p.pattern(v, f);
                let sq = p.seq(&[pat]);
                (vec![sq], vec![])
            },
            &[("abcx", Some((0, "abc"))), ("abc\\0x", Some((0, "abc\\0")))],
        );
        // allowing unrecognized escape sequences
        check(
            |p| {
                let f = p.intern("");
                // Escaped forward slash (used in JS because '/' is the regex delimiter)
                let v1 = p.intern(r"\/");
                // Escaped quotes
                let v2 = p.intern(r#"\"\'"#);
                // Quote preceded by a literal backslash
                let v3 = p.intern(r"[\\']+");
                (
                    vec![p.pattern(v1, f), p.pattern(v2, f), p.pattern(v3, f)],
                    vec![],
                )
            },
            &[
                ("/", Some((0, "/"))),
                ("\"\'", Some((1, "\"\'"))),
                (r"'\'a", Some((2, r"'\'"))),
            ],
        );
        // unicode property escapes
        check(
            |p| {
                let f = p.intern("");
                let v1 = p.intern(r"\p{L}+\P{L}+");
                let v2 = p.intern(r"\p{White_Space}+\P{White_Space}+[\p{White_Space}]*");
                (vec![p.pattern(v1, f), p.pattern(v2, f)], vec![])
            },
            &[
                ("  123   abc", Some((1, "  123   "))),
                ("ბΨƁ___ƀƔ", Some((0, "ბΨƁ___"))),
            ],
        );
        // unicode property escapes in bracketed sets
        check(
            |p| {
                let (v, f) = (p.intern(r"[\p{L}\p{Nd}]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("abΨ12٣٣, ok", Some((0, "abΨ12٣٣")))],
        );
        // unicode character escapes
        check(
            |p| {
                let f = p.intern("");
                let v1 = p.intern(r"\u{00dc}");
                let v2 = p.intern(r"\U{000000dd}");
                let v3 = p.intern(r"\u00de");
                let v4 = p.intern(r"\U000000df");
                (
                    vec![
                        p.pattern(v1, f),
                        p.pattern(v2, f),
                        p.pattern(v3, f),
                        p.pattern(v4, f),
                    ],
                    vec![],
                )
            },
            &[
                ("\u{00dc}", Some((0, "\u{00dc}"))),
                ("\u{00dd}", Some((1, "\u{00dd}"))),
                ("\u{00de}", Some((2, "\u{00de}"))),
                ("\u{00df}", Some((3, "\u{00df}"))),
            ],
        );
        check(
            |p| {
                let f = p.intern("");
                let v1 = p.intern(r"u\{[0-9a-fA-F]+\}");
                // Already-escaped curly braces
                let v2 = p.intern(r"\{[ab]{3}\}");
                // Unicode codepoints
                let v3 = p.intern(r"\u{1000A}");
                // Unicode codepoints (lowercase)
                let v4 = p.intern(r"\u{1000b}");
                (
                    vec![
                        p.pattern(v1, f),
                        p.pattern(v2, f),
                        p.pattern(v3, f),
                        p.pattern(v4, f),
                    ],
                    vec![],
                )
            },
            &[
                ("u{1234} ok", Some((0, "u{1234}"))),
                ("{aba}}", Some((1, "{aba}"))),
                ("\u{1000A}", Some((2, "\u{1000A}"))),
                ("\u{1000b}", Some((3, "\u{1000b}"))),
            ],
        );
        // Case-insensitive patterns must not fold in the two non-ASCII code
        // points that Unicode simple case folding maps onto ASCII letters:
        // `ſ` (U+017F) onto `s`, and the Kelvin sign `K` (U+212A) onto `k`.
        check(
            |p| {
                let (v, f) = (p.intern("[sk]+"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("sSkK.", Some((0, "sSkK"))),
                ("\u{017f}", None),              // long s, not matched by `s`
                ("\u{212a}", None),              // Kelvin sign, not matched by `k`
                ("sk\u{212a}", Some((0, "sk"))), // folded code point ends the token
            ],
        );
        // A broad class carrying `/i` (a negated class, `\p{L}`, ...) keeps
        // `ſ`/`K`: folding never *introduces* them here (the class already
        // contains them), so there is nothing to drop.
        check(
            |p| {
                let (v, f) = (p.intern("[^\"]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("\u{017f}", Some((0, "\u{017f}"))), // long s kept under /i
                ("\u{212a}", Some((0, "\u{212a}"))), // Kelvin sign kept under /i
                ("s", Some((0, "s"))),
            ],
        );
        // An intentionally-written `ſ`/`K` is preserved: the stripping above
        // only fires when the ASCII pair it folds with is also present.
        check(
            |p| {
                let (v, f) = (p.intern("[\u{017f}\u{212a}]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("\u{017f}\u{212a}.", Some((0, "\u{017f}\u{212a}")))],
        );
        // Without the `i` flag nothing is folded, so a broad class such as
        // `[^"]` must keep `ſ`/`K` instead of stripping them as fold artifacts.
        check(
            |p| {
                let (v, f) = (p.intern("[^\"]"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("a", Some((0, "a"))),
                ("\u{017f}", Some((0, "\u{017f}"))), // long s
                ("\u{212a}", Some((0, "\u{212a}"))), // Kelvin sign
                ("\"", None),                        // the one excluded character
            ],
        );
        // `ſ`/`K` fold with nothing, so an explicit one pulls in no ASCII letter.
        check(
            |p| {
                let (v, f) = (p.intern("[\u{017f}\u{212a}]+"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("\u{017f}\u{212a}.", Some((0, "\u{017f}\u{212a}"))),
                ("s", None),
                ("k", None),
            ],
        );
        // `\p{L}` already contains `ſ`/`K`, so folding adds nothing to strip.
        check(
            |p| {
                let (v, f) = (p.intern(r"\p{L}+"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("\u{017f}", Some((0, "\u{017f}"))),
                ("\u{212a}", Some((0, "\u{212a}"))),
                ("aA", Some((0, "aA"))),
                ("1", None),
            ],
        );
        // Folding happens at the leaves, before negation: `[^a-z]` under `/i`
        // must exclude `A-Z`, not re-admit it.
        check(
            |p| {
                let (v, f) = (p.intern("[^a-z]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("!", Some((0, "!"))),
                ("a", None),
                ("A", None),
                ("\u{017f}", Some((0, "\u{017f}"))),
                ("\u{212a}", Some((0, "\u{212a}"))),
            ],
        );
        // The same ordering through a nested class and through class-set algebra.
        check(
            |p| {
                let (v, f) = (p.intern("[^[a-c]]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("d", Some((0, "d"))), ("a", None), ("A", None), ("C", None)],
        );
        check(
            |p| {
                let (v, f) = (p.intern("[[a-z]--[b-d]]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("a", Some((0, "a"))),
                ("A", Some((0, "A"))),
                ("b", None),
                ("B", None),
            ],
        );
        check(
            |p| {
                let (v, f) = (p.intern("[[a-z]&&[b-d]]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("b", Some((0, "b"))),
                ("B", Some((0, "B"))),
                ("e", None),
                ("E", None),
            ],
        );
        // Scoped flags apply only where they are in effect.
        check(
            |p| {
                let (v, f) = (p.intern("(?i)a(?-i)b"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("ab", Some((0, "ab"))),
                ("Ab", Some((0, "Ab"))),
                ("aB", None),
            ],
        );
        // Case-insensitivity does not leak from one token to the next.
        check(
            |p| {
                let (v1, f1) = (p.intern("ab"), p.intern("i"));
                let (v2, f2) = (p.intern("cd"), p.intern(""));
                let s = p.intern("ef");
                (
                    vec![p.pattern(v1, f1), p.pattern(v2, f2), p.string(s)],
                    vec![],
                )
            },
            &[
                ("AB", Some((0, "AB"))),
                ("cd", Some((1, "cd"))),
                ("CD", None),
                ("ef", Some((2, "ef"))),
                ("EF", None),
            ],
        );
        check(
            |p| {
                let (v, f) = (p.intern(r"\$\{[a-z0-9_\.]*[^a-z0-9_\.\}]"), p.intern("i"));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("${a}", None),
                ("${A}", None),
                ("${a!", Some((0, "${a!"))),
                ("${!", Some((0, "${!"))),
            ],
        );
        // `(?-u:...)` matches bytes. A byte below `0x80` is its own code point,
        // so an ASCII byte class converts exactly.
        check(
            |p| {
                let (v, f) = (p.intern(r"(?-u:[a-z])+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("abc.", Some((0, "abc"))), ("ABC", None)],
        );
        // Byte folding is ASCII only, so it cannot pull in `K`.
        check(
            |p| {
                let (v, f) = (p.intern(r"(?-u:(?i)k)+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[("kK.", Some((0, "kK"))), ("\u{212a}", None)],
        );
        // Emojis
        check(
            |p| {
                let (v, f) = (p.intern(r"\p{Emoji}+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("🐎", Some((0, "🐎"))),
                ("🐴🐴", Some((0, "🐴🐴"))),
                ("#0", Some((0, "#0"))), // These chars are technically emojis!
                ("⻢", None),
                ("♞", None),
                ("horse", None),
            ],
        );
        // Intersection
        check(
            |p| {
                let (v, f) = (p.intern(r"[[0-7]&&[4-9]]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("456", Some((0, "456"))),
                ("64", Some((0, "64"))),
                ("452", Some((0, "45"))),
                ("91", None),
                ("8", None),
                ("3", None),
            ],
        );
        // Difference
        check(
            |p| {
                let (v, f) = (p.intern(r"[[0-9]--[4-7]]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("123", Some((0, "123"))),
                ("83", Some((0, "83"))),
                ("9", Some((0, "9"))),
                ("124", Some((0, "12"))),
                ("67", None),
                ("4", None),
            ],
        );
        // Symmetric difference
        check(
            |p| {
                let (v, f) = (p.intern(r"[[0-7]~~[4-9]]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("123", Some((0, "123"))),
                ("83", Some((0, "83"))),
                ("9", Some((0, "9"))),
                ("124", Some((0, "12"))),
                ("67", None),
                ("4", None),
            ],
        );
        // Nested set operations
        check(
            //               0 1 2 3 4 5 6 7 8 9
            // [0-5]:        y y y y y y
            // [2-4]:            y y y
            // [0-5]--[2-4]: y y       y
            // [3-9]:              y y y y y y y
            // [6-7]:                    y y
            // [3-9]--[5-7]:       y y y     y y
            // final regex:  y y   y y       y y
            |p| {
                let (v, f) = (p.intern(r"[[[0-5]--[2-4]]~~[[3-9]--[6-7]]]+"), p.intern(""));
                (vec![p.pattern(v, f)], vec![])
            },
            &[
                ("01", Some((0, "01"))),
                ("432", Some((0, "43"))),
                ("8", Some((0, "8"))),
                ("9", Some((0, "9"))),
                ("2", None),
                ("567", None),
            ],
        );
    }

    #[test]
    fn test_separators_are_expanded_once() {
        //   tok0: 'a',  tok1: 'b',  tok2: 'c',  separator: ' '
        // The per-token copies each carry their own copy of the separator; the shared NFA
        // carries one, and every token that can be preceded by a separator starts there.
        let mut pool = RulePool::default();
        let (a, b, c, space) = (
            pool.intern("a"),
            pool.intern("b"),
            pool.intern("c"),
            pool.intern(" "),
        );
        let roots = [a, b, c].map(|s| pool.string(s)).to_vec();
        let separators = vec![pool.string(space)];
        let vars = roots
            .into_iter()
            .enumerate()
            .map(|(i, root)| LexicalToken {
                name: pool.intern(&format!("tok{i}")),
                kind: VariableType::Anonymous,
                root,
            })
            .collect::<Vec<_>>();
        let grammar = expand_tokens(&mut pool, &vars, &separators).unwrap();

        let shared_start = grammar.shared_start.clone();
        assert_eq!(shared_start[0], shared_start[1]);
        assert_eq!(shared_start[0], shared_start[2]);
        assert_eq!(grammar.shared_owner[shared_start[0] as usize], u32::MAX);

        // Three token bodies plus one separator block, instead of three copies of it.
        assert!(grammar.shared_nfa.states.len() < grammar.nfa.states.len());

        for &(input, expected) in &[
            ("a", Some((0, "a"))),
            ("  b", Some((1, "b"))),
            ("   c", None),
            ("  c", Some((2, "c"))),
        ] {
            assert_eq!(
                simulate_shared(&grammar, &[expected.map_or(0, |(i, _)| i)], input),
                expected,
                "input {input}"
            );
        }
    }

    #[test]
    fn test_shared_nfa_has_no_separators_without_extras() {
        // With no extras there is nothing to share.
        let mut pool = RulePool::default();
        let roots = ["x", "y"]
            .map(|s| {
                let s = pool.intern(s);
                pool.string(s)
            })
            .to_vec();
        let vars = roots
            .into_iter()
            .enumerate()
            .map(|(i, root)| LexicalToken {
                name: pool.intern(&format!("tok{i}")),
                kind: VariableType::Anonymous,
                root,
            })
            .collect::<Vec<_>>();
        let grammar = expand_tokens(&mut pool, &vars, &[]).unwrap();
        // With no extras there is no shared prefix: each token starts in its own body.
        assert_eq!(grammar.shared_start.len(), 2);
        assert_ne!(grammar.shared_start[0], grammar.shared_start[1]);
        assert_eq!(simulate_shared(&grammar, &[1], "y"), Some((1, "y")));
        assert_eq!(simulate_shared(&grammar, &[0, 1], " z"), None);
    }

    #[test]
    fn test_non_ascii_byte_class_is_rejected() {
        let mut pool = RulePool::default();
        let (v, f) = (pool.intern(r"(?-u:[\xc3\xa9])"), pool.intern(""));
        let root = pool.pattern(v, f);
        let vars = vec![LexicalToken {
            name: pool.intern("tok"),
            kind: VariableType::Anonymous,
            root,
        }];
        assert_eq!(
            expand_tokens(&mut pool, &vars, &[]).unwrap_err(),
            ExpandTokensError::Processing(ExpandTokensProcessingError {
                rule: "tok".into(),
                error: ExpandRuleError::ExpandRegex(ExpandRegexError::NonAsciiByteClass(
                    NonAsciiByteClassError {
                        start: 0xa9,
                        end: 0xa9
                    }
                ))
            })
        );
    }

    #[test]
    fn test_repeat_of_empty_choice_does_not_leave_an_accept_state() {
        check(
            |p| {
                let (a, b) = (p.intern("a"), p.intern("b"));
                let token_a = p.string(a);
                let (left, right) = (p.blank(), p.blank());
                let empty_choice = p.choice(&[left, right]);
                let repeated_empty = p.repeat(empty_choice);
                let token_b_suffix = p.string(b);
                let token_b = p.seq(&[repeated_empty, token_b_suffix]);
                (vec![token_a, token_b], vec![])
            },
            &[("b", Some((1, "b")))],
        );
    }
}
