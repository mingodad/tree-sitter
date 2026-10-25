use std::hash::BuildHasher as _;

use hashbrown::{HashTable, hash_table};
use rustc_hash::{FxBuildHasher, FxHashMap};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    grammars::{
        InputGrammar, ProdRef, Production, ProductionStep, ProductionStore, SyntaxGrammar,
        SyntaxVariable,
    },
    prepare_grammar::extract_tokens::ExtractedGrammarMeta,
    rules::{Alias, Associativity, Precedence, Rule, RuleId, RulePool, Symbol, TokenSet},
    strpool::{StrId, StrPool},
};

pub type FlattenGrammarResult<T> = Result<T, FlattenGrammarError>;

#[derive(Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum FlattenGrammarError {
    #[error("No such reserved word set: {0}")]
    NoReservedWordSet(Box<str>),
    #[error("Reserved word set count {0} exceeds the maximum of {max}", max = u16::MAX)]
    TooManyReservedWordSets(usize),
    #[error("{0}")]
    EpsilonCycle(Box<str>),
    #[error("Rule `{0}` cannot be inlined because it contains a reference to itself")]
    RecursiveInline(Box<str>),
    #[error("Rule `{0}` has no reachable productions.")]
    NoReachableProductions(Box<str>),
}

#[derive(Clone, Copy, Default)]
struct FlattenCtx {
    prec: Precedence,
    assoc: Option<Associativity>,
    alias: Option<Alias>,
    field: Option<StrId>,
    reserved: u16,
}

/// Selects one branch at each choice encountered during a root-to-leaf walk. After
/// a production is emitted, `advance` increments this path like a mixed-radix counter.
/// Later decisions are discarded because choosing an earlier branch can expose a
/// different set of nested choices.
#[derive(Default)]
struct ChoiceCursor {
    decisions: Vec<u32>,
    arities: Vec<u32>,
    depth: usize,
}

impl ChoiceCursor {
    /// Begin a new walk for a new variable.
    fn reset(&mut self) {
        self.decisions.clear();
        self.arities.clear();
        self.depth = 0;
    }

    /// Begin a new walk for the same variable as a previous walk
    fn begin_path(&mut self) {
        self.arities.clear();
        self.depth = 0;
    }

    /// Called at each choice. Selects which path to take, and records the decision.
    fn select(&mut self, len: u32) -> u32 {
        debug_assert!(len > 0);
        let selected = self.decisions.get(self.depth).copied().unwrap_or(0);
        debug_assert!(selected < len);
        self.arities.push(len);
        self.depth += 1;
        selected
    }

    /// Find the latest path that we can advance and discard every decision after it.
    /// Returns whether a new path could be found.
    fn advance(&mut self) -> bool {
        for depth in (0..self.arities.len()).rev() {
            let selected = self.decisions.get(depth).copied().unwrap_or(0);
            if selected + 1 < self.arities[depth] {
                self.decisions.resize(depth + 1, 0);
                self.decisions[depth] = selected + 1;
                return true;
            }
        }
        false
    }
}

/// Reusable scratch for enumerating and flattening one production path at a time.
#[derive(Default)]
pub(super) struct FlattenState {
    steps: Vec<ProductionStep>,
    choices: ChoiceCursor,
    dyn_prec: i32,
    dead: bool,
    /// The positions in [`ProductionStore::productions`] of the current variable's
    /// productions, by the hash of their [`ProdRef`].
    emitted: HashTable<u32>,
}

impl FlattenState {
    fn reset_variable(&mut self) {
        self.choices.reset();
        self.emitted.clear();
        self.reset_path();
    }

    fn reset_path(&mut self) {
        self.steps.clear();
        self.dyn_prec = 0;
        self.dead = false;
        self.choices.begin_path();
    }

    fn push_step(&mut self, symbol: Symbol, ctx: FlattenCtx) {
        self.steps.push(ProductionStep::pack(
            symbol,
            ctx.prec,
            ctx.assoc,
            ctx.alias,
            ctx.field,
            ctx.reserved,
        ));
    }

    fn restore_outer_prec(&mut self, outer: Precedence) {
        let step = self.steps.last_mut().unwrap();
        step.set_precedence(outer);
    }

    fn restore_outer_assoc(&mut self, outer: Option<Associativity>) {
        let step = self.steps.last_mut().unwrap();
        step.set_associativity(outer);
    }
}

/// Flatten one deterministic path. Choices only select a child, the outer loop in
/// the caller enumerates subsequent paths from the root.
fn apply(
    pool: &RulePool,
    reserved_ids: &FxHashMap<StrId, u16>,
    node: RuleId,
    f_ctx: FlattenCtx,
    at_end: bool,
    st: &mut FlattenState,
) -> FlattenGrammarResult<bool> {
    match pool.node(node) {
        Rule::Sym(symbol) => {
            st.push_step(symbol, f_ctx);
            Ok(true)
        }
        Rule::Seq(range) => {
            let children = pool.child_slice(range);
            let mut did_push = false;
            for (i, &child) in children.iter().enumerate() {
                did_push |= apply(
                    pool,
                    reserved_ids,
                    child,
                    f_ctx,
                    at_end && i + 1 == children.len(),
                    st,
                )?;
                if st.dead {
                    break;
                }
            }
            Ok(did_push)
        }
        Rule::Choice(range) => {
            // An empty choice matches nothing, so no production can contain it.
            if range.len == 0 {
                st.dead = true;
                return Ok(false);
            }
            let selected = st.choices.select(range.len);
            let child = pool.child_slice(range)[selected as usize];
            apply(pool, reserved_ids, child, f_ctx, at_end, st)
        }
        Rule::Eof => {
            st.push_step(Symbol::End, f_ctx);
            Ok(true)
        }
        Rule::Metadata { params, rule } => {
            let params = pool.params(params);
            let mut inner_ctx = f_ctx;
            if params.precedence != Precedence::None {
                inner_ctx.prec = params.precedence;
            }
            if params.associativity.is_some() {
                inner_ctx.assoc = params.associativity;
            }
            if params.alias.is_some() {
                inner_ctx.alias = params.alias;
            }
            if params.field.is_some() {
                inner_ctx.field = params.field;
            }
            if params.dynamic_precedence.abs() > st.dyn_prec.abs() {
                st.dyn_prec = params.dynamic_precedence;
            }

            let did_push = apply(pool, reserved_ids, rule, inner_ctx, at_end, st)?;
            // A step's prec/assoc governs the parse position just _after_ it, so the
            // regions's last step owns the gap past it. If more steps follow, that gap
            // is outside the region and reverts to the outer context. At the production's
            // tail the gap is the reduce and keeps the region's own value.
            if did_push && !at_end {
                if params.precedence != Precedence::None {
                    st.restore_outer_prec(f_ctx.prec);
                }
                if params.associativity.is_some() {
                    st.restore_outer_assoc(f_ctx.assoc);
                }
            }
            Ok(did_push)
        }
        Rule::Reserved { rule, ctx } => {
            let Some(&reserved) = reserved_ids.get(&ctx) else {
                return Err(FlattenGrammarError::NoReservedWordSet(
                    pool.resolve(ctx).to_string().into(),
                ));
            };
            let inner = FlattenCtx { reserved, ..f_ctx };
            apply(pool, reserved_ids, rule, inner, at_end, st)
        }
        _ => Ok(false),
    }
}

/// Append the completed path as a production unless this variable already has an
/// identical one. A path with `eof()` anywhere but the final step is discarded,
/// since such a production could never be completed.
fn emit(st: &mut FlattenState, out: &mut ProductionStore) -> bool {
    let last = st.steps.len().saturating_sub(1);
    let Some(eof_index) = st
        .steps
        .iter()
        .position(|step| step.symbol() == Symbol::End)
    else {
        return emit_ready(st, out, false);
    };
    if eof_index != last {
        return false;
    }
    st.steps.pop();
    emit_ready(st, out, true)
}

fn emit_ready(
    st: &mut FlattenState,
    out: &mut ProductionStore,
    requires_eof_lookahead: bool,
) -> bool {
    let production = ProdRef {
        steps: &st.steps,
        dynamic_precedence: st.dyn_prec,
        requires_eof_lookahead,
    };
    let entry = st.emitted.entry(
        FxBuildHasher.hash_one(production),
        |&index| out.production(index) == production,
        |&index| FxBuildHasher.hash_one(out.production(index)),
    );
    let hash_table::Entry::Vacant(entry) = entry else {
        // This variable already has an identical production.
        return true;
    };
    entry.insert(out.productions.len() as u32);
    let steps_start = out.steps.len() as u32;
    out.steps.extend_from_slice(&st.steps);
    out.productions.push(Production {
        steps_start,
        steps_len: st.steps.len() as u32,
        dynamic_precedence: st.dyn_prec,
        requires_eof_lookahead,
    });
    true
}

pub(super) fn flatten_grammar(
    g: &InputGrammar,
    meta: &ExtractedGrammarMeta,
    st: &mut FlattenState,
    out: &mut ProductionStore,
) -> FlattenGrammarResult<()> {
    if meta.reserved_sets.len() > usize::from(u16::MAX) {
        Err(FlattenGrammarError::TooManyReservedWordSets(
            meta.reserved_sets.len(),
        ))?;
    }
    // Last wins on duplicate names.
    let reserved_ids: FxHashMap<StrId, u16> = meta
        .reserved_sets
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (*name, i as u16))
        .collect();
    for v in &g.variables {
        let prod_start = out.productions.len() as u32;
        let mut dropped_for_eof = false;
        st.reset_variable();
        loop {
            apply(
                &g.pool,
                &reserved_ids,
                v.root,
                FlattenCtx::default(),
                true,
                st,
            )?;
            if !st.dead {
                dropped_for_eof |= !emit(st, out);
            }
            if !st.choices.advance() {
                break;
            }
            st.reset_path();
        }
        if dropped_for_eof && prod_start == out.productions.len() as u32 {
            return Err(FlattenGrammarError::NoReachableProductions(
                g.pool.resolve(v.name).to_string().into(),
            ));
        }
        out.var_prods
            .push((prod_start, out.productions.len() as u32));
    }
    check(g, meta, out)
}

/// Post-flatten checks: no rule may reach itself without consuming input, and no
/// recursive inlines.
fn check(
    g: &InputGrammar,
    meta: &ExtractedGrammarMeta,
    out: &ProductionStore,
) -> FlattenGrammarResult<()> {
    check_epsilon_cycles(g, out)?;
    for (i, &(p_start, p_end)) in out.var_prods.iter().enumerate() {
        let symbol = Symbol::non_terminal(i);
        if !meta.inline.contains(&symbol) {
            continue;
        }
        for p in &out.productions[p_start as usize..p_end as usize] {
            if out.steps[p.step_range()]
                .iter()
                .any(|s| s.symbol() == symbol)
            {
                Err(FlattenGrammarError::RecursiveInline(
                    g.pool.resolve(g.variables[i].name).to_string().into(),
                ))?;
            }
        }
    }
    Ok(())
}

/// Reject the one thing an empty rule cannot be allowed to do: depend on another
/// empty rule that depends back.
///
/// A rule that matches nothing is a reduction with no children, which the parse
/// table handles — the start rule has always been able to do it, and now every
/// rule can.  What no table can handle is a *circle* of them: `a: b?; b: ();`
/// reduces `b`, then `a`, then asks for `b` again at the same position, forever.
/// The runtime would spin without consuming input, so this is the failure mode
/// that must be caught here rather than diagnosed later.
///
/// `nullable[i]` means variable `i` has a production all of whose steps can
/// match nothing.  `i` can then hand over to `j` — an edge `i -> j` — when one
/// of its productions contains `j` and every *other* step of that production is
/// nullable too, because that is a path from `i` to `j` that consumes nothing.
/// A cycle among those edges is an empty cycle.
fn check_epsilon_cycles(g: &InputGrammar, out: &ProductionStore) -> FlattenGrammarResult<()> {
    let count = out.var_prods.len();
    let mut nullable = vec![false; count];
    let mut changed = true;
    while changed {
        changed = false;
        for (i, &(p_start, p_end)) in out.var_prods.iter().enumerate() {
            if nullable[i] {
                continue;
            }
            let can_be_empty = out.productions[p_start as usize..p_end as usize]
                .iter()
                .any(|p| {
                    !p.requires_eof_lookahead
                        && out.steps[p.step_range()].iter().all(|step| {
                        step.symbol()
                            .non_terminal_index()
                            .and_then(|j| nullable.get(usize::from(j)))
                            .copied()
                            .unwrap_or(false)
                    })
                });
            if can_be_empty {
                nullable[i] = true;
                changed = true;
            }
        }
    }

    // Only a nullable variable can hand over to another without consuming.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (i, &(p_start, p_end)) in out.var_prods.iter().enumerate() {
        if !nullable[i] {
            continue;
        }
        for p in &out.productions[p_start as usize..p_end as usize] {
            if p.requires_eof_lookahead {
                continue;
            }
            let steps = &out.steps[p.step_range()];
            for (at, step) in steps.iter().enumerate() {
                let Some(j) = step.symbol().non_terminal_index() else {
                    continue;
                };
                let j = usize::from(j);
                if !nullable.get(j).copied().unwrap_or(false) {
                    continue;
                }
                let rest_is_empty = steps.iter().enumerate().all(|(other, step)| {
                    other == at
                        || step
                            .symbol()
                            .non_terminal_index()
                            .and_then(|k| nullable.get(usize::from(k)))
                            .copied()
                            .unwrap_or(false)
                });
                if rest_is_empty {
                    edges[i].push(j);
                }
            }
        }
    }

    // Iterative depth-first search; the grammar can have a thousand variables and
    // this must not depend on the stack.
    const WHITE: u8 = 0;
    const GREY: u8 = 1;
    const BLACK: u8 = 2;
    let mut colour = vec![WHITE; count];
    let mut path: Vec<(usize, usize)> = Vec::new();
    for root in 0..count {
        if colour[root] != WHITE || !nullable[root] {
            continue;
        }
        colour[root] = GREY;
        path.push((root, 0));
        while let Some(&mut (node, ref mut next)) = path.last_mut() {
            let Some(&to) = edges[node].get(*next) else {
                colour[node] = BLACK;
                path.pop();
                continue;
            };
            *next += 1;
            match colour[to] {
                WHITE => {
                    colour[to] = GREY;
                    path.push((to, 0));
                }
                GREY => {
                    let start = path.iter().position(|(v, _)| *v == to).unwrap();
                    let mut names: Vec<&str> = path[start..]
                        .iter()
                        .map(|(v, _)| g.pool.resolve(g.variables[*v].name))
                        .collect();
                    names.sort_unstable();
                    names.dedup();
                    Err(FlattenGrammarError::EpsilonCycle(epsilon_cycle_message(
                        &names,
                    )))?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The text of [`FlattenGrammarError::EpsilonCycle`].
///
/// One rule and several rules fail for the same reason but read badly in a
/// single sentence: `Rule \`a\` can each match the empty string through one
/// another` says nothing. The names arrive sorted and deduplicated.
fn epsilon_cycle_message(names: &[&str]) -> Box<str> {
    let list = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>();
    let Some((last, rest)) = list.split_last() else {
        return Box::from("a rule matches the empty string in a circle");
    };
    let text = if rest.is_empty() {
        format!(
            "Rule {last} can match the empty string and reach itself without consuming a\n\
             token.\n\
             \n\
             That reduction consumes no input, so the parser would repeat it at the same\n\
             position, forever. Break the cycle: make it consume a token, or remove the\n\
             empty alternative."
        )
    } else {
        format!(
            "Rules {} and {last} can each match the empty string only by going through the\n\
             others.\n\
             \n\
             Those rules therefore reduce in a circle that consumes no input, and the parser\n\
             would reduce forever at the same position. Break the cycle: make one of them\n\
             consume a token, or remove one of the empty alternatives.",
            rest.join(", ")
        )
    };
    text.into_boxed_str()
}

pub(super) fn assemble_syntax_grammar(
    g: InputGrammar,
    meta: ExtractedGrammarMeta,
    out: ProductionStore,
) -> (SyntaxGrammar, StrPool) {
    let mut reserved_word_sets = meta
        .reserved_sets
        .iter()
        .map(|(_, symbols)| symbols.iter().copied().collect())
        .collect::<Vec<_>>();
    if reserved_word_sets.is_empty() {
        reserved_word_sets.push(TokenSet::default());
    }
    let variables = g
        .variables
        .iter()
        .zip(&meta.kinds)
        .map(|(v, &kind)| SyntaxVariable { name: v.name, kind })
        .collect::<Vec<_>>();

    let interner = g.pool.into_interner();

    (
        SyntaxGrammar {
            variables,
            extra_symbols: meta.extra_symbols,
            expected_conflicts: meta.conflicts,
            external_tokens: meta.external_tokens,
            supertype_symbols: meta.supertypes,
            variables_to_inline: meta.inline,
            word_token: meta.word,
            precedence_orderings: g.precedence_orderings,
            reserved_word_sets,

            steps: out.steps,
            productions: out.productions,
            var_prods: out.var_prods,
        },
        interner,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammars::{Variable, VariableType};

    #[test]
    fn test_flatten_grammar() {
        let (grammar, interner) = flatten_named(|p| {
            let nt1 = non_term(p, 1);
            let nt2 = non_term(p, 2);
            let nt3 = non_term(p, 3);
            let nt4 = non_term(p, 4);
            let nt5 = non_term(p, 5);
            let nt6 = non_term(p, 6);
            let nt7 = non_term(p, 7);
            let inner = {
                let s = p.seq(&[nt3, nt4]);
                p.prec_right(Precedence::Integer(102), s)
            };
            let choice = p.choice(&[inner, nt5]);
            let pl = {
                let s = p.seq(&[nt2, choice, nt6]);
                p.prec_left(Precedence::Integer(101), s)
            };
            p.seq(&[nt1, pl, nt7])
        })
        .unwrap();

        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![
                ProdView {
                    dyn_prec: 0,
                    steps: vec![
                        StepView::new(Symbol::non_terminal(1)),
                        StepView::new(Symbol::non_terminal(2))
                            .prec(Precedence::Integer(101))
                            .assoc(Some(Associativity::Left)),
                        StepView::new(Symbol::non_terminal(3))
                            .prec(Precedence::Integer(102))
                            .assoc(Some(Associativity::Right)),
                        StepView::new(Symbol::non_terminal(4))
                            .prec(Precedence::Integer(101))
                            .assoc(Some(Associativity::Left)),
                        StepView::new(Symbol::non_terminal(6)),
                        StepView::new(Symbol::non_terminal(7)),
                    ]
                },
                ProdView {
                    dyn_prec: 0,
                    steps: vec![
                        StepView::new(Symbol::non_terminal(1)),
                        StepView::new(Symbol::non_terminal(2))
                            .prec(Precedence::Integer(101))
                            .assoc(Some(Associativity::Left)),
                        StepView::new(Symbol::non_terminal(5))
                            .prec(Precedence::Integer(101))
                            .assoc(Some(Associativity::Left)),
                        StepView::new(Symbol::non_terminal(6)),
                        StepView::new(Symbol::non_terminal(7)),
                    ]
                },
            ]
        );
    }

    #[test]
    fn test_flatten_grammar_with_maximum_dynamic_precedence() {
        let (grammar, interner) = flatten_named(|p| {
            let nt1 = non_term(p, 1);
            let nt2 = non_term(p, 2);
            let nt3 = non_term(p, 3);
            let nt4 = non_term(p, 4);
            let nt5 = non_term(p, 5);
            let nt6 = non_term(p, 6);
            let nt7 = non_term(p, 7);
            let inner = {
                let s = p.seq(&[nt3, nt4]);
                p.prec_dynamic(102, s)
            };
            let choice = p.choice(&[inner, nt5]);
            let pd = {
                let s = p.seq(&[nt2, choice, nt6]);
                p.prec_dynamic(101, s)
            };
            p.seq(&[nt1, pd, nt7])
        })
        .unwrap();

        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![
                ProdView {
                    dyn_prec: 102,
                    steps: vec![
                        StepView::new(Symbol::non_terminal(1)),
                        StepView::new(Symbol::non_terminal(2)),
                        StepView::new(Symbol::non_terminal(3)),
                        StepView::new(Symbol::non_terminal(4)),
                        StepView::new(Symbol::non_terminal(6)),
                        StepView::new(Symbol::non_terminal(7)),
                    ]
                },
                ProdView {
                    dyn_prec: 101,
                    steps: vec![
                        StepView::new(Symbol::non_terminal(1)),
                        StepView::new(Symbol::non_terminal(2)),
                        StepView::new(Symbol::non_terminal(5)),
                        StepView::new(Symbol::non_terminal(6)),
                        StepView::new(Symbol::non_terminal(7)),
                    ]
                },
            ]
        );
    }

    #[test]
    fn test_flatten_grammar_with_final_precedence() {
        let (grammar, interner) = flatten_named(|p| {
            let nt1 = non_term(p, 1);
            let nt2 = non_term(p, 2);
            let s = p.seq(&[nt1, nt2]);
            p.prec_left(Precedence::Integer(101), s)
        })
        .unwrap();
        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![ProdView {
                dyn_prec: 0,
                steps: vec![
                    StepView::new(Symbol::non_terminal(1))
                        .prec(Precedence::Integer(101))
                        .assoc(Some(Associativity::Left)),
                    StepView::new(Symbol::non_terminal(2))
                        .prec(Precedence::Integer(101))
                        .assoc(Some(Associativity::Left)),
                ]
            }]
        );

        let (grammar, interner) = flatten_named(|p| {
            let nt1 = non_term(p, 1);
            let s = p.seq(&[nt1]);
            p.prec_left(Precedence::Integer(101), s)
        })
        .unwrap();
        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![ProdView {
                dyn_prec: 0,
                steps: vec![
                    StepView::new(Symbol::non_terminal(1))
                        .prec(Precedence::Integer(101))
                        .assoc(Some(Associativity::Left)),
                ]
            }]
        );
    }

    #[test]
    fn test_flatten_grammar_with_field_names() {
        let (grammar, interner) = flatten_named(|p| {
            let t1 = term(p, 1);
            let f1 = {
                let n = p.intern("first-thing");
                p.field(n, t1)
            };
            let t2 = term(p, 2);
            let t3 = term(p, 3);
            let f2 = {
                let n = p.intern("second-thing");
                p.field(n, t3)
            };
            let blank = p.blank();
            let choice = p.choice(&[blank, f2]);
            p.seq(&[f1, t2, choice])
        })
        .unwrap();

        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![
                ProdView {
                    dyn_prec: 0,
                    steps: vec![
                        StepView::new(Symbol::terminal(1)).field("first-thing"),
                        StepView::new(Symbol::terminal(2)),
                    ]
                },
                ProdView {
                    dyn_prec: 0,
                    steps: vec![
                        StepView::new(Symbol::terminal(1)).field("first-thing"),
                        StepView::new(Symbol::terminal(2)),
                        StepView::new(Symbol::terminal(3)).field("second-thing"),
                    ]
                },
            ]
        );
    }

    #[test]
    fn test_precedence_inherited_through_inner_metadata() {
        // A prec region inherited by an inner symbol that carries _different_ metadata
        // (field).
        let (grammar, interner) = flatten_named(|p| {
            let a = term(p, 1);
            let b = term(p, 2);
            let bf = {
                let n = p.intern("f");
                p.field(n, b)
            };
            let s = p.seq(&[a, bf]);
            p.prec(Precedence::Integer(5), s)
        })
        .unwrap();

        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![ProdView {
                dyn_prec: 0,
                steps: vec![
                    StepView::new(Symbol::terminal(1)).prec(Precedence::Integer(5)),
                    StepView::new(Symbol::terminal(2))
                        .prec(Precedence::Integer(5))
                        .field("f"),
                ]
            }]
        );
    }

    #[test]
    fn test_flatten_grammar_with_recursive_inline_variable() {
        let mut pool = RulePool::default();
        let nt0 = non_term(&mut pool, 0);
        let nt1 = non_term(&mut pool, 1);
        let nt2 = non_term(&mut pool, 2);
        let root = pool.seq(&[nt0, nt1, nt2]);
        let name = pool.intern("test");
        let pg = InputGrammar {
            pool,
            variables: vec![Variable { name, root }],
            ..Default::default()
        };
        let meta = ExtractedGrammarMeta {
            kinds: vec![VariableType::Named],
            inline: vec![Symbol::non_terminal(0)],
            ..Default::default()
        };
        assert_eq!(
            run(pg, meta).unwrap_err(),
            FlattenGrammarError::RecursiveInline("test".to_string().into())
        );
    }

    #[test]
    fn test_flatten_grammar_with_unknown_reserved() {
        // Unknown reserved context
        let err = flatten_named(|p| {
            let t1 = term(p, 1);
            let ctx = p.intern("nope");
            p.reserved(t1, ctx)
        })
        .unwrap_err();
        assert_eq!(
            err,
            FlattenGrammarError::NoReservedWordSet("nope".to_string().into())
        );
    }

    /// A rule used in the middle of another rule may match nothing: `a` is one
    /// step of nothing and then a token, which the table reduces once and then
    /// shifts past.
    #[test]
    fn test_flatten_grammar_with_empty_production() {
        let mut pool = RulePool::default();
        let inner = non_term(&mut pool, 1);
        let tail = term(&mut pool, 2);
        let a_root = pool.seq(&[inner, tail]);
        let b_root = pool.blank();
        let (a, b) = (pool.intern("a"), pool.intern("b"));
        let pg = InputGrammar {
            pool,
            variables: vec![
                Variable {
                    name: a,
                    root: a_root,
                },
                Variable {
                    name: b,
                    root: b_root,
                },
            ],
            ..Default::default()
        };
        let meta = ExtractedGrammarMeta {
            kinds: vec![VariableType::Named, VariableType::Named],
            ..Default::default()
        };
        let (grammar, interner) = run(pg, meta).expect("an empty rule in sequence is legal");
        assert_eq!(
            prods(&grammar, 1, &interner),
            vec![ProdView {
                dyn_prec: 0,
                steps: vec![],
            }]
        );
    }

    #[test]
    fn test_flatten_grammar_with_empty_production_cycle() {
        // `a: b; b: a | ();` — each can be empty only through the other.
        let mut pool = RulePool::default();
        let a_root = non_term(&mut pool, 1);
        let b_to_a = non_term(&mut pool, 0);
        let b_blank = pool.blank();
        let b_root = pool.choice(&[b_to_a, b_blank]);
        let (a, b) = (pool.intern("a"), pool.intern("b"));
        let pg = InputGrammar {
            pool,
            variables: vec![
                Variable {
                    name: a,
                    root: a_root,
                },
                Variable {
                    name: b,
                    root: b_root,
                },
            ],
            ..Default::default()
        };
        let meta = ExtractedGrammarMeta {
            kinds: vec![VariableType::Named, VariableType::Named],
            ..Default::default()
        };
        let err = run(pg, meta).unwrap_err();
        assert_eq!(
            err,
            FlattenGrammarError::EpsilonCycle(epsilon_cycle_message(&["a", "b"]))
        );
        // One sentence has to name both rules, not say "can each" about one.
        let text = err.to_string();
        assert!(text.starts_with("Rules `a` and `b`"), "{text}");
        assert!(
            text.contains("reduce forever at the same position"),
            "{text}"
        );
    }

    /// The shape `a: b*; b: ();` reaches the same place through the auxiliary
    /// variable that `expand_repeats` builds (`aux: choice(seq(aux, aux), b)`),
    /// so the cycle check has to see through it.
    #[test]
    fn test_flatten_grammar_with_repeat_of_an_empty_rule() {
        let mut pool = RulePool::default();
        let aux = non_term(&mut pool, 1);
        let two = pool.seq(&[aux, aux]);
        let inner = non_term(&mut pool, 2);
        let aux_root = pool.choice(&[two, inner]);
        let a_blank = pool.blank();
        let a_root = pool.choice(&[aux, a_blank]);
        let b_blank = pool.blank();
        let (a, aux_name, b) = (pool.intern("a"), pool.intern("a_repeat1"), pool.intern("b"));
        let pg = InputGrammar {
            pool,
            variables: vec![
                Variable {
                    name: a,
                    root: a_root,
                },
                Variable {
                    name: aux_name,
                    root: aux_root,
                },
                Variable {
                    name: b,
                    root: b_blank,
                },
            ],
            ..Default::default()
        };
        let meta = ExtractedGrammarMeta {
            kinds: vec![
                VariableType::Named,
                VariableType::Auxiliary,
                VariableType::Named,
            ],
            ..Default::default()
        };
        let err = run(pg, meta).unwrap_err();
        assert_eq!(
            err,
            FlattenGrammarError::EpsilonCycle(epsilon_cycle_message(&["a_repeat1"]))
        );
        let text = err.to_string();
        assert!(text.starts_with("Rule `a_repeat1` can match"), "{text}");
        assert!(text.contains("reach itself"), "{text}");
    }

    #[test]
    fn test_flatten_grammar_with_empty_choice() {
        let (grammar, interner) = flatten_named(|p| {
            let prefix = term(p, 1);
            let empty = p.choice(&[]);
            let tail_a = term(p, 2);
            let tail_b = term(p, 3);
            let tail = p.choice(&[tail_a, tail_b]);
            let dead = p.seq(&[prefix, empty, tail]);
            let live = term(p, 4);
            p.choice(&[dead, live])
        })
        .unwrap();

        assert_eq!(
            prods(&grammar, 0, &interner),
            vec![ProdView {
                dyn_prec: 0,
                steps: vec![StepView::new(Symbol::terminal(4))],
            }]
        );

        let (grammar, interner) = flatten_named(|p| p.choice(&[])).unwrap();
        assert_eq!(prods(&grammar, 0, &interner), [] as [ProdView; 0]);
    }

    fn term(p: &mut RulePool, i: u32) -> RuleId {
        p.push_node(Rule::from(Symbol::terminal(i as usize)))
    }
    fn non_term(p: &mut RulePool, i: u32) -> RuleId {
        p.push_node(Rule::from(Symbol::non_terminal(i as usize)))
    }

    #[derive(Debug, PartialEq)]
    struct StepView {
        symbol: Symbol,
        prec: Precedence,
        assoc: Option<Associativity>,
        alias: Option<Alias>,
        field: Option<String>,
        reserved: u16,
    }

    impl StepView {
        fn new(symbol: Symbol) -> Self {
            Self {
                symbol,
                prec: Precedence::None,
                assoc: None,
                alias: None,
                field: None,
                reserved: 0,
            }
        }
        fn prec(mut self, prec: Precedence) -> Self {
            self.prec = prec;
            self
        }
        fn assoc(mut self, assoc: Option<Associativity>) -> Self {
            self.assoc = assoc;
            self
        }
        fn field(mut self, name: &str) -> Self {
            self.field = Some(name.to_string());
            self
        }
    }

    #[derive(Debug, PartialEq)]
    struct ProdView {
        dyn_prec: i32,
        steps: Vec<StepView>,
    }

    fn run(
        g: InputGrammar,
        meta: ExtractedGrammarMeta,
    ) -> FlattenGrammarResult<(SyntaxGrammar, StrPool)> {
        let mut st = FlattenState::default();
        let mut out = ProductionStore::default();
        flatten_grammar(&g, &meta, &mut st, &mut out)?;
        Ok(assemble_syntax_grammar(g, meta, out))
    }

    fn flatten_named(
        build: impl FnOnce(&mut RulePool) -> RuleId,
    ) -> FlattenGrammarResult<(SyntaxGrammar, StrPool)> {
        let mut pool = RulePool::default();
        let root = build(&mut pool);
        let name = pool.intern("test");
        let pg = InputGrammar {
            pool,
            variables: vec![Variable { name, root }],
            ..Default::default()
        };
        let meta = ExtractedGrammarMeta {
            kinds: vec![VariableType::Named],
            ..Default::default()
        };
        run(pg, meta)
    }

    /// Unpack a variable's productions from the pooled output into comparable views.
    fn prods(grammar: &SyntaxGrammar, var: usize, interner: &StrPool) -> Vec<ProdView> {
        let (p_start, p_end) = grammar.var_prods[var];
        grammar.productions[p_start as usize..p_end as usize]
            .iter()
            .map(|p| ProdView {
                dyn_prec: p.dynamic_precedence,
                steps: grammar.steps[p.step_range()]
                    .iter()
                    .map(|&s| StepView {
                        symbol: s.symbol(),
                        prec: s.precedence(),
                        assoc: s.associativity(),
                        alias: s.alias(),
                        field: s.field().map(|f| interner.resolve(f).to_string()),
                        reserved: s.reserved,
                    })
                    .collect(),
            })
            .collect()
    }
}
