# DELTA — code vs `ARCHITECTURE.md`

[`ARCHITECTURE.md`](ARCHITECTURE.md) states the settled design.
This document records where the code diverges from it, as of 2026-07-30, across the four repos that exist: `git-anchor` (here), `../git-store`, `../git-query`, `../git-forge`.

Two of the six products have no code at all: `git-attest` and `git-effect`.

Verdicts: **MATCHES** — code implements the design.
**DELTA** — code exists and disagrees.
**ABSENT** — design element has no implementation.

---

## Cross-cutting

| # | design | reality | verdict |
|---|---|---|---|
| X1 | six products; `git-attest` is a repo sibling to `git-anchor` | no `../git-attest` repo, no `gix-attest`/`git-attest` crate anywhere. A trailer-based claim store lives in `../git-query/crates/gix-query-host/src/claim.rs` (`claim/1`, `kind/2`, `target/2`, `signer/2`, `verdict/2`) with no envelope, no chaining, no crypto | ABSENT |
| X2 | `git-effect` lives in query's repo until the rule language settles | only a gate-rule skeleton naming `effect_ref`/`effect_admin_violation` (`../git-query/crates/gix-query-kernel/src/lib.rs:32-35,85,161-167`). No effect doc schema, write-set declarations, trigger machinery, or `git effect` CLI | ABSENT |
| X3 | the identity normal form: a frozen mini-codec over a closed primitive universe (scalars, byte strings, hashes, lists, maps) with a type-universe check, owned by store | no such codec or check exists in `../git-store`. Identity subtrees are hashed through the general `facet-git-tree` codec | ABSENT |
| X4 | store's write path carries opaque signature bytes via a `Signer` trait, never interpreted | no `Signer` trait. `gix-refstore`'s `Committer` supplies git author/committer identity only (`../git-store/crates/gix-refstore/src/store.rs:58-73`, `gix-store/src/store.rs:251-267`) — identity metadata, not signature bytes | ABSENT |
| X5 | `Action` record as a store typed doc (`key: {executor, inputs, params}`, `output`); its hash is also the query cache key and effect idempotence key | no `Action` schema or type in any repo | ABSENT |
| X6 | op-log records are store typed docs — codec in store, base-fact predicates in query, consumed by anchor's op-log oracle | no op-log schema in store, no op-log predicate in query, no op-log oracle in anchor. All three legs missing | ABSENT |

`X3`–`X6` are substrate: `X3` and `X4` are prerequisites for almost everything downstream, and `X6` is one design element split across three repos, currently unimplemented in all three.

## `git-anchor`

| # | design | reality | verdict |
|---|---|---|---|
| A1 | `hints: { fingerprints: [{algo, params, value}], descriptors: [{grammar id+version, node kind, name path}] }` | `hints: { blob, content, context }` — blob oid, full blob bytes, and a context window (`crates/gix-anchor/src/anchor.rs:56-68`) | DELTA |
| A2 | `identity.span` is a byte range over post-clean-filter blob bytes, always canonical | `identity.lines` is a `LineRange`, 1-based inclusive (`crates/gix-anchor/src/anchor.rs:24-30,49-50`) | DELTA |
| A3 | three oracles as pure fns of `(objects, Binding, params)`: op-log, diff-trace, fingerprint | `projection.rs:210-540` has `project`, `project_exact`, `project_from_context`, `project_many`, `project_candidates`. None of the three named oracles exists; no `(oracle, confidence)` on results | DELTA |
| A4 | `project` is library-internal; no user-facing command resolves through it | `project` is `pub` (`crates/gix-anchor/src/projection.rs`) | DELTA |
| A5 | anchor registers the `rebind pin` payload as a store schema | no schema registration in `crates/gix-anchor` | ABSENT |
| A6 | `Binding` is `identity` + `hints` sibling subtrees | it is (`crates/gix-anchor/src/anchor.rs:40-68`) — contents differ per `A1`/`A2` | MATCHES |
| A7 | anchor id = store hash of the identity subtree alone | `crates/gix-anchor/src/handle.rs:98-105` serializes the identity subtree only | MATCHES |
| A8 | `git anchor create` is a pure emitter, advances nothing | `crates/git-anchor/src/main.rs:221-231` | MATCHES |
| A9 | anchor holds no policy or thresholds; no dependency on attest or query | confirmed; `gix-anchor` depends only on facet, facet-git-tree, gix, gix-object, thiserror | MATCHES |
| A10 | forge-layer comment crates live here, move at code-freeze | already moved to `../git-forge` (commit `b237358`). The doc's "Current layout note" is stale | DELTA (doc) |

`A7` is a partial match: the right subtree is hashed, but through the general codec rather than the normal form (`X3`).

## `git-store`

| # | design | reality | verdict |
|---|---|---|---|
| S1 | CLI: `git store put <schema> <value>`, `get <tree-ish>`, `check <tree-ish> <schema>` | `put <kind> [name]`, `get <kind> <name>`, `schema put/get/show/log`; no `check` (`../git-store/crates/git-store/src/main.rs:39-58`). The CLI is kind/name-addressed, the doc is schema/tree-ish-addressed | DELTA |
| S2 | identity-normal-form enforcement at schema registration (so action `params` is expressible in it) | `Schema::put` accepts any schema, no type-universe check (`../git-store/crates/gix-store/src/kind.rs:343-345`) | ABSENT |
| S3 | `facet-git-tree` is the pure codec, dynamic facet values for JSON-like objects; document identity is the compiled tree hash | as designed (`../git-store/crates/facet-git-tree/src/lib.rs:1-43`) | MATCHES |
| S4 | the dynamic write path is library API reusable by other CLIs | `serialize_value_with_schema` and `DocumentBuilder` are public; interactive prompting sits in the CLI over them (`facet-git-tree/src/lib.rs:40`, `gix-store/src/document.rs:37-95`, `git-store/src/interactive.rs:20-30`) | MATCHES |
| S5 | one hash function, no key crates | `gix_hash::ObjectId` throughout, no separate key derivation | MATCHES |
| S6 | store does not decide authority, derive, or set ref-advance policy | confirmed; `commit_forward` does CAS-and-retry without policy | MATCHES |

## `git-query`

| # | design | reality | verdict |
|---|---|---|---|
| Q1 | `bind/5` — `bind(A, Rev, Loc, O, C)` — is the only user-facing resolution, over one confidence lattice with pin as an oracle at 1.0, max-selection in its own aggregation stratum | `bind/7` — `(Anchor, Rev, Blob, Start, End, Position, Content)` — calls `project_exact`/`project_from_context` directly, emits no oracle label and no confidence, and there is no `pin_claim` predicate or lattice (`../git-query/crates/gix-query-host/src/bind.rs:1-122`) | DELTA |
| Q2 | validation is 9 passes **plus** effect-stratification over declared read/write namespace sets | exactly 9 passes, `pass1_symbols`…`pass9_mint` (`../git-query/crates/gix-query-check/src/lib.rs:84-95`); no stratification pass | ABSENT |
| Q3 | results go to one of three places: ephemeral, cache ref under `refs/query/cache/*`, promotion to claim/effect | ephemeral stdout only (`../git-query/crates/git-query/src/main.rs:225-229`) | DELTA |
| Q4 | Nemo behind an engine-agnostic seam; naive evaluator retained as oracle | Nemo lowering is direct, no seam trait; no naive evaluator (`../git-query/crates/gix-query-eval/src/lib.rs:4-12`) | DELTA |
| Q5 | EDB includes op-log records, typed docs from store, and extension-contributed predicates (e.g. `cst_node/4`) | registry has commit/parent/tree_entry/author/member/revoked/claim/kind/target/signer/verdict/anchor/line (`../git-query/crates/gix-query-ir/src/registry.rs:517-656`); the three named groups are absent | DELTA |
| Q6 | CLI: `run <rule> [args]`, `explain <rule> [args]`, `rule <subcommands>` | `run`, `predicates`, `rules add\|list\|check`; `explain` is not wired to the CLI (`../git-query/crates/git-query/src/main.rs:22-58`) | DELTA |
| Q7 | rule modules: one per ref under `refs/meta/rules/*`, with `pub` visibility markers | as designed (`../git-query/crates/gix-query-rules/src/store.rs:12-14`, `gix-query-ir/src/rule.rs:139-176`, `gix-query-parse/src/parser.rs:246-258`) | MATCHES |
| Q8 | magic-set demand loop for moded builtins | present (`../git-query/crates/gix-query-eval/src/rewrite.rs`, `demand.rs`) | MATCHES |

## `git-forge`

| # | design | reality | verdict |
|---|---|---|---|
| F1 | comment = forge doc embedding a `Binding` subtree inline | `add_comment`/`add_anchored_comment` are TODO stubs (`../git-forge/crates/gix-forge/src/lib.rs:461-488`); the moved `gix-comment` code is not wired in | ABSENT |
| F2 | authorship, edit history, and body belong to the document | `CommentEdit` exists (`../git-forge/crates/gix-forge/src/lib.rs:215-250`) but is unused pending `F1` | DELTA |
| F3 | merge gates = effects gated on `reviewed` + policy claims | none — blocked on `X2` | ABSENT |
| F4 | issues, agent branches, web UI | `Issue` and `Review` types exist (`../git-forge/crates/gix-forge/src/lib.rs:67-417`); no agent branches, no web UI | DELTA |
| F5 | review targets: blob, commit, tree/subtree, commit range, (base, tip) pairs | `ReviewTarget` covers all five (`../git-forge/crates/gix-forge/src/lib.rs:424-431`) | MATCHES |
| F6 | `reviewed/1` is a derived query predicate at blob granularity | Datalog rule `reviewed(B) :- approved_by(B, _)` registered with query (`../git-forge/crates/gix-forge/src/lib.rs:298`) | MATCHES |
| F7 | forge reads binds through query only | `gix-forge` depends on `gix-query`, not `gix-anchor` | MATCHES |
| F8 | forge owns no primitive logic | no hashing, signing, or ref-transition authority of its own | MATCHES |

---

## Status

The table above is the audit as taken.
This section records what has since been closed against it.

| item | closed by | repo |
|---|---|---|
| `X3` identity normal form | `fe4a5f9` | `git-store` |
| `X4` `Signer` seam | `898482b` | `git-store` |
| `S2` identity-universe check at registration | `281ace1` | `git-store` |
| `S1` CLI shape | `1a46310`, `7474c92` | `git-store` |
| `A1` fingerprints and descriptors | `ad8ef55` | `git-anchor` |
| `A2` span over post-clean-filter bytes | `ad8ef55` | `git-anchor` |
| `A3` the three named oracles | `8d7cc5b` | `git-anchor` |
| `A4` `project` made library-internal | `8d7cc5b` | `git-anchor` |
| `A5` `rebind pin` schema registration | `8d7cc5b` | `git-anchor` |
| `A7` anchor id hashed through the normal form | `8d7cc5b` | `git-anchor` |
| `Q2` effect-stratification pass | `1a8418e` | `git-query` |
| `Q6` CLI shape | `3be4f21` | `git-query` |
| `Q1` `bind/5` over one confidence lattice | `ba4362de42aba2d9458a2011f57f1d8a81e2ed40`, `e6bb2be` | `git-query` |

`Q1` landed in two steps.
The first replaced `bind/7` with `bind/5` but computed both candidate generation and max-selection inside one Rust builtin, which is not the design: with no materialized `cand` relation the engine can hold neither semi-naive incrementality over candidate generation nor per-anchor recomputation of selection, and sub-maximal candidates stay unreachable from the rule language, so no rule can express the orphaning threshold of `ARCHITECTURE.md`'s line 242.
`e6bb2be` split them — `cand/5` is the monotone host builtin, `bind/5` is a rule in the core module.
`max{}` has no lowering path in the IR, so selection is the standard stratified-negation encoding of argmax over a numeric confidence column, which needs nothing the engine does not already run.

Deliberately out of scope: `X1` (`git-attest`), `X2` (`git-effect`), `X5` (`Action` record), `X6` (op-log), and `F1`–`F4` (forge).
`Q1`'s pin leg therefore reads the existing trailer-based claim store rather than a signed attest envelope, and the op-log oracle contributes no candidates for want of a source.
Still open in scope: `Q3`, `Q4`, `Q5`.

`A10` is unchanged: `ARCHITECTURE.md`'s "Current layout note" still places the comment crates in this repo.

## Dependency order for closing the gap

1. **`X3`, `X4` in store** — the identity normal form and the `Signer` seam. `A2`, `A7`, `S2`, `X5`, `X6` all sit on top of them.
2. **`A1`, `A2`, `A3` in anchor** — hints reshaped to fingerprints/descriptors, span moved from lines to post-clean-filter bytes, the three named oracles. `A2` is a breaking identity change; nothing durable depends on current ids.
3. **`X1` — `git-attest`** — a new repo. `Q1`'s pin leg, `F3`, and every promotion path need it.
4. **`Q1`, `Q2`, `Q3`** — the confidence lattice, the stratification pass, cache refs.
5. **`X2`, `X5`, `F1`, `F3`** — effect, action records, and the forge comment path.
