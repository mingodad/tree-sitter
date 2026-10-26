# Which of the fork's patches are independent of `tsg`

Companion to `UPSTREAM.md` (which lists *findings*). This one lists the *code
deltas* and can be read without knowing anything about `tsg`.

## The shape of the fork

19 commits on `wip` since the import, **+2996 / −191 in 16 files**:

```
$ git diff --stat <base>..wip -- crates      → 16 files, +2996 −191
$ git diff --stat <base>..wip -- lib         → (empty)
```

So: every change is in `crates/generate` and `crates/cli`. **The C runtime and
the ABI are untouched**, which means none of these can break an already-generated
parser — the main reason a maintainer can afford to look at them at all. Only one
commit (`ce61d7c`) even knows `tsg` exists.

Caveat: this repo is a flattened copy of upstream with no shared history, so a
diff against upstream has to be done by hand. Everything below is a commit to
cherry-pick, not a patch series.

## Tier 1 — bug fixes. Send first, no tsg in the report

| commit | size | what is wrong today |
|---|---|---|
| `5ff59a5` | 18 | A re-queued LALR state **appends** to its action entries instead of replacing them; only `debug_assert` notices, so release builds emit two designated initializers for one cell and C keeps the last. PostgreSQL 16: **494 KB of duplicates**, `-Winitializer-overrides` on every one. |
| `0000f4d` | 313 | Lookaheads are computed from the item's step list, not from the suffix: an item `int_opt alias` gets `FIRST(alias)` and never learns that `int_opt` can consume the token first. Wrong reduce lookaheads ⇒ conflicts that aren't real. |
| `491bac2` | 49 | FIRST/LAST derived only from a production's first and last step; middle steps ignored ⇒ ERROR/MISSING nodes on input that the grammar accepts. Fix had to reorder the nullability fixpoint. |
| `1ff1cf4` | 109 | A repeat whose item begins zero-width (`(a?)*`) is expanded into a rule whose recursion consumes nothing. |

These four are honest bugs against `grammar.json` input. They need one thing
before filing: **a `grammar.js` repro**, because our repros are `.tsg` files. That
is mechanical (dump the `.tsg` to `grammar.json`, write the equivalent JS), and
`--dump-expanded` makes it easy, but it has not been done.

## Tier 2 — performance, the strongest thing in the fork

On the PostgreSQL 16 grammar (1195 rules, 489 tokens, 7509 productions):

| commit | size | measured |
|---|---|---|
| `b829c6b` | 53 | `NfaCursor::add_states` was quadratic (binary-search insert per state, full-worklist scan per child). Generation stamps + one sort/dedup. Lexing that grammar alone took **>25 s, 89 % of `generate`**. |
| `a3e4498` | 358 | `expand_tokens` built one NFA per token, each `sep* token`, so a grammar with N tokens carried N copies of the separator. One shared separator with a hub over token bodies: **14.1k → 3.4k lex states, `build_lex_table` >25 s → 0.5 s, peak RSS 942 MB → 87 MB**. |
| `22a5636` | 78 | The token conflict matrix is pairwise-independent: one row block per thread, per-thread `NfaCursor`. **12.3 s → 3.2 s**, results bit-identical. |

Together: `generate` for a PostgreSQL-sized grammar went from "over half a
minute and a gigabyte" to **~4 s and under 100 MB**. These are the ones where the
patch argues for itself.

## Tier 3 — diagnostics. Upstream-shaped, but taste is the maintainer's

| commit | size | what it adds |
|---|---|---|
| `01b1463` | 173 | report **every** unresolved conflict, not just the first (a 16-conflict grammar needed 16 runs). |
| `d29e0d2` | 113 | reject unknown keys in `grammar.json`; warn when an extra is shadowed by a token. `grammar.json` is a public format and unknown keys were silently ignored. |
| `49aa685` | 178 | report rules that can never appear in a tree. |
| `8a65d0b` | 452 | report the lexer states and the transitions they drop. |
| `c2b80d8` | 438 | `--dump-expanded`: print the grammar the tables are built from (see `UPSTREAM.md` §3). |
| `7e75cdb` | 49 | generation timings through `log`. |

## Tier 4 — ask before writing code

These encode a decision we made, not an obvious bug. Offer them as questions
with evidence, not as PRs.

| commit | size | the decision |
|---|---|---|
| `025108c` | 33 | let a grammar declare its `start` rule in `grammar.json`. Format addition; the CLI does this today by rewriting the JSON. |
| `56865ab` | 717 | allow nullable rules, and refuse only a **cycle** of empty rules (Kosaraju SCC over the empty-rule graph, reported with the participating rules); EOF-pinned productions excluded from the nullable fixpoint because matching at end of input is not "nothing". Stock refuses far more than that. |
| `69142a0` | 46 | rank an undeclared precedence below a declared one; today an undeclared rule at level 0 beats a declared negative level, which produced 20 ERROR nodes on PostgreSQL (`SET` eaten as a column alias). |
| `b363428` | 253 | `--lalr` (merge states with the same kernel core, union lookaheads) plus `--max-states` and progress reporting. Needed to benchmark like-for-like; useful as a diagnostic, probably not as a default. |

## Tier 5 — not upstreamable as-is

`ce61d7c` (173 lines): accept `grammar.tsg` by running an external front end and
using its stdout as the grammar. The generic form of it is 15 lines and is in
`UPSTREAM.md` §8: dispatch `load_grammar_file` on more than `.js`/`.json`.

## What every patch is missing

- **Tests in upstream's style.** The fork added 3 tests for 3000 lines. Our
  evidence is different in kind — 16 grammars byte-identical to reference
  parsers, 92 parsers round-tripped — which is convincing *evidence* but not a
  thing a maintainer can run. Each Tier 1/2 commit needs one fixture
  (`test/fixtures/…`) and one golden file.
- **A changelog line and a docs touch**, for the Tier 3 flags.
- **A minimal repro per bug.** Only `5ff59a5` has one that is small
  (`postgres16`), and even that wants a smaller grammar.

## What to lead with

1. `5ff59a5` — 18 lines, undeniable, already measured (byte-identical tree
   before/after, 108 generate tests pass).
2. The three perf commits — with the table above and the offer to attach
   `postgres16/grammar.json` (7509 productions, 489 tokens) as a **stress-case
   corpus**, not as a patch.
3. `c2b80d8` as a proposal, since it is what makes the rest of the reports
   readable.

Everything else can wait for a reply.

---

## Branches prepared for review

Two branches exist in `/Volumes/RamDisk/tree-sitter`; neither is pushed.

**`tsg`** (`7ad35eb`, identical to `wip`) — everything: 55 commits, 842 files,
196 of them under `tmp/`. This is what `./push-fork.sh` pushes as the new branch.
Short version of what it contains is in `HANDOFF.md`.

**`upstream`** (`f19579d`) — the 18 generator/CLI commits and nothing else:

```
$ git diff --stat 0fff6ab..upstream
 16 files changed, 2851 insertions(+), 188 deletions(-)
$ git ls-tree -r --name-only upstream | grep -c '^tmp/'
0
```

It was rebuilt from the import commit by applying each patch **restricted to
`crates/` and `lib/`**, because two of the commits rode along with `tmp/`
documentation edits (`0000f4d` also edited `ROADMAP.md`, which does not exist on
that branch and made a plain `git cherry-pick` conflict). That is why the SHAs
differ from `wip` and every message ends with
`(cherry picked from commit <sha>, generator changes only)`.

The `.tsg` seam (`ce61d7c`) is deliberately **not** in it — it is the only commit
that needs `tsg`, and its generic form is `UPSTREAM.md` §8.

`git grep tsg upstream -- crates` finds exactly one hit: a doc comment in the
`--stamp` commit that names "a front end like `tsg`" as an example. Harmless; if
it bothers a reviewer it is one `sed` and a rebase of six commits.

**Review aids, already generated:**

- `tmp/tsg/.review/00*.patch` — the 18 commits as `git format-patch` output
  (248 KB), which is also the shape `git send-email` wants.
- `UPSTREAM.md` — the findings, with what still needs reduction before filing.
- `UPSTREAM-PATCHES.md` — this file: the tiers, and why each patch is or is not
  independent of `tsg`.

**Not rebuilt on the branch, on purpose:** the same code compiled as part of
`wip` (the only commit that could reference something absent is the `.tsg` seam,
which is excluded), and a `cargo check` on the branch would only re-verify that.
If you want that verification anyway: 1–2 min, ~250 MB in `.tsbuild-check`,
delete it afterwards.

**Ordering:** `upstream` follows the order things happened — perf, then
diagnostics, then the four bug fixes. A maintainer usually wants fixes first; the
same 18 patches reorder cheaply (`git rebase -i 0fff6ab`, move the blocks) if you
decide to submit rather than just push.
