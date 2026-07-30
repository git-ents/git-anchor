# DELTA — code vs `ARCHITECTURE.md`

[`ARCHITECTURE.md`](ARCHITECTURE.md) states the settled design.
This document records where the code diverges from it, as of 2026-07-31, across the five repos that have code: `git-anchor` (here), `../git-store`, `../git-query`, `../git-forge`, `../git-attest` (lives inside this repo, `crates/gix-attest` + `crates/git-attest`, per `DEVPLAN-attest.md`).

One of the six products has no code at all: `git-effect`.
`git-effect`'s planned home is `../git-query`'s repo, not a separate sibling — see `DEVPLAN-effect.md` and `X2` below.

Verdicts: **MATCHES** — code implements the design.
**DELTA** — code exists and disagrees.
**ABSENT** — design element has no implementation.

---

## Cross-cutting

| # | design | reality | verdict |
|---|---|---|---|
| X1 | six products; `git-attest` is a repo sibling to `git-anchor` | `git-attest` lives as `crates/gix-attest` (library) and `crates/git-attest` (CLI) inside this repo rather than a separate sibling — the doc's own fallback for exactly this case (`DEVPLAN-attest.md`: "colocation is not coupling," same move as `git-effect`'s planned home). Envelope (`crates/gix-attest/src/envelope.rs`), chaining on `refs/claims/<target-key>` (`crates/gix-attest/src/chain.rs`), crypto verification against SSHSIG-armored `gpgsig` commits (`crates/gix-attest/src/verify.rs`), key add/rotate lifecycle (`crates/gix-attest/src/key.rs`), and a `git attest sign\|revoke\|verify\|log\|resolve` CLI (`crates/git-attest/src/main.rs`) are all implemented. A CI check enforces no `gix-anchor` ↔ `gix-attest` dependency edge (`d0b2855`-equivalent commit `03b367f` in this repo, `d55d0ad` for the sibling-isolation extension). `../git-query`'s claim EDB is re-backed onto it and the trailer store is deleted (`../git-query` commit `63776af`; `../git-query/crates/gix-query-host/src/claim.rs` now derives `claim/1`, `kind/2`, `target/2`, `signer/2`, `verdict/2` from `gix-attest`'s chains, with no trailer parsing left) | MATCHES |
| X2 | `git-effect` lives in query's repo until the rule language settles | still no `gix-effect`/`git-effect` crate anywhere. What now exists in `../git-query`: `EffectDecl`/`Namespace` with prefix-aware `overlaps` (`../git-query/crates/gix-query-ir/src/effect.rs:1-40`), pass 10 effect-stratification over *computed* read footprints (pass 7's output) crossed with *declared* write sets, exported as a public acyclicity check (`../git-query/crates/gix-query-check/src/pass10_effect_stratify.rs`, wired at `../git-query/crates/gix-query-check/src/lib.rs:110`), the parser's `effect NAME writes(...)` clause with `reads(...)` deliberately rejected (`../git-query/crates/gix-query-parse/src/parser.rs:278-292,625-645`), and the kernel's push-gate relations `effect_ref`/`effect_admin_violation` naming `refs/meta/effects/*` (`../git-query/crates/gix-query-kernel/src/lib.rs:85,162-167`). Still missing, per `../git-query/DEVPLAN-effect.md` Part B: the `EffectDoc` schema and `git effect define`, trigger detection (delta-restricted re-evaluation on ref advance, D3), push-intent refs (`refs/intent/*`, D4), idempotence keys (D5), the `Executor` trait and write-set boundary check (D6), the `Action` schema registration (D7), the signed transition log (`refs/effects/log`, D8), and the `git effect` CLI (`define`/`status`/`log`) | ABSENT (narrowed) |
| X3 | the identity normal form: a frozen mini-codec over a closed primitive universe (scalars, byte strings, hashes, lists, maps) with a type-universe check, owned by store | implemented: `../git-store/crates/facet-git-tree/src/normal_form/` is the frozen mini-codec, exercised by `../git-store/crates/facet-git-tree/tests/normal_form.rs` and `tests/normalization.rs` (commit `fe4a5f9`). Identity- and key-bearing subtrees (anchor identity, `crates/gix-anchor/src/anchor.rs` via `facet_git_tree::normal_form::NormalForm`) are hashed through it instead of the general `facet-git-tree` codec | MATCHES |
| X4 | store's write path carries opaque signature bytes via a `Signer` trait, never interpreted | implemented: `Signer`/`SignatureBytes`/`ErasedSigner` in `../git-store/crates/gix-refstore/src/signer.rs:17-81` (exported at `gix-refstore/src/lib.rs:54`, commit `898482b`), wired into `gix-store`'s `write_commit`, which signs the commit bytes and stores the result as an armored block in the standard `gpgsig` header rather than a bespoke hex header (`../git-store` commit `de18290`). Store never interprets the bytes; `gix-attest` is the first and only consumer that reads them back and verifies | MATCHES |
| X5 | `Action` record as a store typed doc (`key: {executor, inputs, params}`, `output`); its hash is also the query cache key and effect idempotence key | no `Action` schema or type in any repo. `../git-query/DEVPLAN-effect.md` D7 assigns registration to `gix-effect` once it exists ("Action is 'not a product,' but a schema needs a registering owner") — still gated on `X2` | ABSENT |
| X6 | op-log records are store typed docs — codec in store, base-fact predicates in query, consumed by anchor's op-log oracle | still genuinely absent: no op-log schema in `../git-store`, no op-log predicate in `../git-query`, no op-log ref format anywhere. The gap is now honestly load-bearing rather than silently missing: `../git-query`'s `key_valid_at/1` is declared against op-log admission order and documents that the op-log does not exist yet, evaluating to `true` for any well-formed claim oid as an explicit absent-safe marker rather than a silent stub (`../git-query/crates/gix-query-host/src/cand.rs:87-121`, doc comment: "was the claim's signing key valid at the op-log position ... That is rule policy ... left open"). `crates/gix-anchor/src/oracle.rs:64-85` likewise takes an `OpLogSource` trait the caller must supply, with no implementation, and its module doc cites this row by name ("no op-log format ... DELTA X6") | ABSENT |

`X3` and `X4` are closed; nothing downstream is blocked on them any longer.
`X5` and `X6` remain substrate gaps: `X5` is blocked on `X2` (`git-effect` needs to exist to register the schema), and `X6` blocks only `key_valid_at`'s real semantics, which is otherwise fully wired to the gap.

## `git-anchor`

| # | design | reality | verdict |
|---|---|---|---|
| A1 | `hints: { fingerprints: [{algo, params, value}], descriptors: [{grammar id+version, node kind, name path}] }` | implemented: `Fingerprint` (`crates/gix-anchor/src/fingerprint.rs`) and the hints subtree carry fingerprints; descriptors as designed. Binding's identity/hints split is `crates/gix-anchor/src/binding.rs` (commit `ad8ef55`, `7c7a0f7`) | MATCHES |
| A2 | `identity.span` is a byte range over post-clean-filter blob bytes, always canonical | implemented: `Span` is a half-open byte range "over a blob's bytes exactly as git stores them (post-clean-filter)" (`crates/gix-anchor/src/anchor.rs:39-56`); `LineRange` survives only as a capture-time UI convenience for `git anchor create -L`, explicitly never durable and never appearing in `AnchorIdentity` (same file, doc comment at `anchor.rs:17-29`) | MATCHES |
| A3 | three oracles as pure fns of `(objects, Binding, params)`: op-log, diff-trace, fingerprint | implemented in `crates/gix-anchor/src/oracle.rs`: `Oracle::{OpLog, DiffTrace, Fingerprint}` (`oracle.rs:34-44`), each producing `Candidate { oracle, confidence, .. }` and applying no threshold. `op_log` (`oracle.rs:74-85`), `diff_trace` (`oracle.rs:98-`), `fingerprint` (`oracle.rs:263-`) | MATCHES |
| A4 | `project` is library-internal; no user-facing command resolves through it | `project` is `pub(crate)` (`crates/gix-anchor/src/oracle.rs:391`), not `pub`; `git-anchor`'s CLI (`crates/git-anchor/src/main.rs`) has no subcommand that calls it — resolution is `../git-query`'s `bind/5` | MATCHES |
| A5 | anchor registers the `rebind pin` payload as a store schema | implemented: `RebindPin` and `register_rebind_pin_schema` in `crates/gix-anchor/src/pin.rs:15-50`, publishing under `REBIND_PIN_KIND = "rebind-pin"` (also exported at the crate root per commit `c133e51`) | MATCHES |
| A6 | `Binding` is `identity` + `hints` sibling subtrees | it is (`crates/gix-anchor/src/binding.rs`) | MATCHES |
| A7 | anchor id = store hash of the identity subtree alone | `crates/gix-anchor/src/handle.rs` hashes the identity subtree through `IdentityNormalForm` (`X3`'s codec), not the general codec — the gap the original row flagged is closed | MATCHES |
| A8 | `git anchor create` is a pure emitter, advances nothing | `crates/git-anchor/src/main.rs:217-228` (`cmd_create`): writes objects, prints a content-addressed handle, advances no ref | MATCHES |
| A9 | anchor holds no policy or thresholds; no dependency on attest or query | confirmed; `crates/gix-anchor/Cargo.toml` depends only on facet, facet-git-tree, gix-store, gix, gix-object, thiserror — no `gix-attest`, no `gix-query` dependency edge (also CI-enforced from attest's side, `X1`) | MATCHES |
| A10 | forge-layer comment crates live here, move at code-freeze | already moved to `../git-forge` (commit `b237358`). `ARCHITECTURE.md`'s "Current layout note" (line 352) is still unchanged and still stale | DELTA (doc) |

`A1`–`A9` are now fully closed; `git-anchor`'s only remaining divergence from the design is the stale prose note in `ARCHITECTURE.md` itself (`A10`), which is a doc-maintenance item, not a code gap.

## `git-store`

| # | design | reality | verdict |
|---|---|---|---|
| S1 | CLI: `git store put <schema> <value>`, `get <tree-ish>`, `check <tree-ish> <schema>` | implemented as designed, plus hidden backward-compatible forms: `put` compiles a value under a schema and prints the tree hash (a second non-JSON argument falls back to the old `put <kind> <name>` ref-addressed form); `get` decodes from any tree-ish of the compiled shape (two arguments falls back to the old `get <kind> <name>` form); `check <tree-ish> <schema>` validates without decoding (`../git-store/crates/git-store/src/main.rs:47-79`, commits `1a46310`, `7474c92`) | MATCHES |
| S2 | identity-normal-form enforcement at schema registration (so action `params` is expressible in it) | implemented: `Schema::put` calls `check_identity_subtrees(doc)` before committing (`../git-store/crates/gix-store/src/kind.rs:375-382,428-`), refusing a schema whose identity subtree leaves the normal form (commit `281ace1`) | MATCHES |
| S3 | `facet-git-tree` is the pure codec, dynamic facet values for JSON-like objects; document identity is the compiled tree hash | as designed (`../git-store/crates/facet-git-tree/src/lib.rs:1-20`) | MATCHES |
| S4 | the dynamic write path is library API reusable by other CLIs | `serialize_value_with_schema` (`../git-store/crates/facet-git-tree/src/schema/write.rs:66`) and `DocumentBuilder` (`../git-store/crates/gix-store/src/document.rs:37`) are public; interactive prompting sits in the CLI over them | MATCHES |
| S5 | one hash function, no key crates | `gix_hash::ObjectId` throughout, no separate key derivation; anchor ids, action keys (once `X5` lands), and attest target keys (`X1`) all reuse this one hash via the normal form | MATCHES |
| S6 | store does not decide authority, derive, or set ref-advance policy | confirmed; `commit_forward` does CAS-and-retry without policy | MATCHES |

`git-store` has no open rows: `S1`–`S6` all match the design.

## `git-query`

| # | design | reality | verdict |
|---|---|---|---|
| Q1 | `bind/5` — `bind(A, Rev, Loc, O, C)` — is the only user-facing resolution, over one confidence lattice with pin as an oracle at 1.0, max-selection in its own aggregation stratum | implemented, in two landed steps. `../git-query` commit `ba4362de42aba2d9458a2011f57f1d8a81e2ed40` replaced `bind/7` with `bind/5`; commit `e6bb2be` split candidate generation from selection: `cand/5` is a monotone host builtin (`../git-query/crates/gix-query-host/src/cand.rs`), and `bind/5` is a rule in the core module deriving `dominated`/`bind` via a stratified-negation argmax encoding, not a Rust-side max (`../git-query/crates/gix-query-rules/src/assemble.rs:30-47`). The pin leg reads `gix-attest` claims (via `X1`'s re-backing) filtered to `payload_kind == "rebind-pin"`, decoded against anchor's `RebindPin` schema (`A5`) — the composition point `ARCHITECTURE.md` names explicitly | MATCHES |
| Q2 | validation is 9 passes **plus** effect-stratification over declared read/write namespace sets | implemented as pass 10, over *computed* read footprints (not declared — `../git-query/DEVPLAN-effect.md` D2 argues a declared read set can lie) crossed with declared write sets: `../git-query/crates/gix-query-check/src/pass10_effect_stratify.rs`, wired after pass 9 in `../git-query/crates/gix-query-check/src/lib.rs:60-110` (commits `1a8418e`, `d0b2855`) | MATCHES (design text says "declared read/write"; code computes reads — see `X2`) |
| Q3 | results go to one of three places: ephemeral, cache ref under `refs/query/cache/*`, promotion to claim/effect | still ephemeral stdout only; no `refs/query/cache/*` writer anywhere in `../git-query/crates` (grep for "refs/query/cache" and "cache ref" returns nothing outside docs). `../git-query/DEVPLAN-effect.md` C5 keeps this as `DEVPLAN.md` Phase 6, explicitly last, and ties promotion to the same cache-key derivation once it lands | DELTA |
| Q4 | Nemo behind an engine-agnostic seam; naive evaluator retained as oracle | still direct: `../git-query/crates/gix-query-eval/src/lib.rs:1-12` states plainly "the only crate permitted to name a Nemo type ... there is no trait here anticipating a second, because a seam with one implementation is a seam whose only client is a test." No naive evaluator. This is an explicit, argued design choice in the crate doc, not an oversight, but it still disagrees with `ARCHITECTURE.md`'s text | DELTA |
| Q5 | EDB includes op-log records, typed docs from store, and extension-contributed predicates (e.g. `cst_node/4`) | registry (`../git-query/crates/gix-query-ir/src/registry.rs`) now includes `cand/5` and `key_valid_at/1` in addition to the prior commit/parent/tree_entry/author/member/revoked/claim/kind/target/signer/verdict/anchor/line set; still no op-log records (`X6`) and no extension-contributed predicate example (`cst_node/4` or equivalent) | DELTA |
| Q6 | CLI: `run <rule> [args]`, `explain <rule> [args]`, `rule <subcommands>` | `run`, `explain`, and `rules {publish, list, check}` all present (`../git-query/crates/git-query/src/main.rs:30-113`); `explain` is now wired (the prior audit's DELTA said it was not) | MATCHES |
| Q7 | rule modules: one per ref under `refs/meta/rules/*`, with `pub` visibility markers | as designed (`../git-query/crates/gix-query-rules/src/store.rs`, `gix-query-ir/src/rule.rs`, `gix-query-parse/src/parser.rs`) | MATCHES |
| Q8 | magic-set demand loop for moded builtins | present and still wired: `mode.rs` (`../git-query/crates/gix-query-ir/src/mode.rs`), `rewrite.rs`/`demand.rs` (`../git-query/crates/gix-query-eval/src/rewrite.rs`, `demand.rs`, called from `engine.rs:301` and re-exported at `lib.rs:55-70`), and pass 8 (`../git-query/crates/gix-query-check/src/pass8_modes.rs`, still invoked at `lib.rs:108-109`). **Note:** `../git-query/DEVPLAN.md` §0 (2026-07-30, commit `9ee4406`) is a *decision* to cut the mode system, pass 8, the magic-set rewrite, and the demand loop entirely in favor of eager per-query footprint materialization plus a two-level extraction/answer cache — but as of this audit that decision has not been executed: all of the named files and passes are still present, still compiled, and still called from the evaluation path. Until that removal lands, this row is a match to the *current* `ARCHITECTURE.md` text and a known-scheduled divergence from the *plan* | MATCHES (scheduled for removal — see note) |

`Q1` and `Q2` are newly closed since the last audit and match cleanly.
`Q3`, `Q4`, `Q5` remain open, as the Status section already anticipated.
`Q8`'s eventual removal (once `../git-query/DEVPLAN.md` §0 is executed) will flip this table's row to ABSENT for the mode system/magic-sets/demand loop and require a corresponding rewrite of `ARCHITECTURE.md`'s git-query section — tracked here so the next audit does not miss it.

## `git-forge`

| # | design | reality | verdict |
|---|---|---|---|
| F1 | comment = forge doc embedding a `Binding` subtree inline | still stubs: `add_comment`/`add_anchored_comment` are `// TODO: implement comment storage` / `// TODO: implement anchored comment storage` (`../git-forge/crates/gix-forge/src/lib.rs:461-489`); the `gix-comment` crate is a dependency (`../git-forge/crates/gix-forge/Cargo.toml`) but not wired into these functions | ABSENT |
| F2 | authorship, edit history, and body belong to the document | `CommentEdit` exists (`../git-forge/crates/gix-forge/src/lib.rs:215`) but is unused pending `F1` | DELTA |
| F3 | merge gates = effects gated on `reviewed` + policy claims | none — blocked on `X2` | ABSENT |
| F4 | issues, agent branches, web UI | `Issue` (`../git-forge/crates/gix-forge/src/lib.rs:67`) and `Review` (`:194`) types exist, with a maturing interactive CLI (recent commits: checkbox pickers, in-place edit, archived issue bodies); no agent branches, no web UI | DELTA |
| F5 | review targets: blob, commit, tree/subtree, commit range, (base, tip) pairs | `ReviewTarget` (`../git-forge/crates/gix-forge/src/lib.rs:424`) covers all five | MATCHES |
| F6 | `reviewed/1` is a derived query predicate at blob granularity | Datalog rules registered with query: `pub reviewed(B).`, `reviewed(B) :- approved_by(B, _).`, plus `unreviewed`/`blocked` derived alongside it (`../git-forge/crates/gix-forge/src/lib.rs:286-311`) | MATCHES |
| F7 | forge reads binds through query only | `gix-forge` depends on `gix-query`, `gix-store`, `gix-comment`, `gix` — not `gix-anchor` (`../git-forge/crates/gix-forge/Cargo.toml`) | MATCHES |
| F8 | forge owns no primitive logic | no hashing, signing, or ref-transition authority of its own | MATCHES |

`git-forge` is unchanged since the last audit: `F1` and `F3` remain genuinely absent, both blocked on upstream work (`F1` needs its own implementation effort; `F3` is blocked on `X2`).

---

## Status

The table above is the audit as taken.
This section records what has since been closed against it.

| item | closed by | repo |
|---|---|---|
| `X1` `git-attest` (envelope, chain, crypto verify, key lifecycle, CLI, query re-backing) | `d69d175`, `b350088`, `4b98ae2`, `ce3fec1`, `03b367f`, `d55d0ad`, `c133e51` (`git-anchor`); `63776af` (`git-query`) | `git-anchor`, `git-query` |
| `X3` identity normal form | `fe4a5f9` | `git-store` |
| `X4` `Signer` seam (+ `gpgsig` transport) | `898482b`, `de18290` | `git-store` |
| `S2` identity-universe check at registration | `281ace1` | `git-store` |
| `S1` CLI shape | `1a46310`, `7474c92` | `git-store` |
| `A1` fingerprints and descriptors | `ad8ef55` | `git-anchor` |
| `A2` span over post-clean-filter bytes | `ad8ef55` | `git-anchor` |
| `A3` the three named oracles | `8d7cc5b` | `git-anchor` |
| `A4` `project` made library-internal | `8d7cc5b` | `git-anchor` |
| `A5` `rebind pin` schema registration | `8d7cc5b` | `git-anchor` |
| `A7` anchor id hashed through the normal form | `8d7cc5b` | `git-anchor` |
| `Q1` `bind/5` over one confidence lattice, with `cand/5` split out | `ba4362de42aba2d9458a2011f57f1d8a81e2ed40`, `e6bb2be` | `git-query` |
| `Q2` effect-stratification pass, over computed footprints | `1a8418e`, `d0b2855` | `git-query` |
| `Q6` CLI shape | `3be4f21` | `git-query` |

`Q1` landed in two steps.
The first replaced `bind/7` with `bind/5` but computed both candidate generation and max-selection inside one Rust builtin, which is not the design: with no materialized `cand` relation the engine can hold neither semi-naive incrementality over candidate generation nor per-anchor recomputation of selection, and sub-maximal candidates stay unreachable from the rule language, so no rule can express the orphaning threshold of `ARCHITECTURE.md`'s line 242.
`e6bb2be` split them — `cand/5` is the monotone host builtin, `bind/5` is a rule in the core module.
`max{}` has no lowering path in the IR, so selection is the standard stratified-negation encoding of argmax over a numeric confidence column, which needs nothing the engine does not already run.

`X1` closed as an in-repo colocation (`crates/gix-attest`, `crates/git-attest` here in `git-anchor`), not the separate `../git-attest` sibling repo the original row imagined — `DEVPLAN-attest.md` argues colocation is not coupling, the same reasoning `git-effect`'s planned home already relies on, and a CI check keeps `gix-anchor` and `gix-attest` from ever depending on each other. `Q1`'s pin leg now reads real signed claims through this envelope rather than the retired trailer store.

`X2` is narrowed, not closed: `../git-query` now has the effect *type* surface (`EffectDecl`, `Namespace`, the parser clause) and the stratification/acyclicity check pass 10 exports, all built against *computed* footprints rather than the doc's "declared read/write namespace sets" text — but no effect *runtime* exists anywhere (no `EffectDoc`, no trigger detection, no executor seam, no transition log, no `git effect` CLI).
See the `git-query` table's `X2` row for the itemized remainder.

Deliberately out of scope: `X2` (`git-effect` runtime), `X5` (`Action` record — blocked on `X2`), `X6` (op-log), and `F1`/`F3` (forge comment storage and merge gates — `F3` blocked on `X2`).
The op-log oracle (`crates/gix-anchor/src/oracle.rs`) contributes no candidates for want of a source, and `key_valid_at` evaluates to an honest, documented `true` pending it.
Still open in scope: `Q3` (cache refs / promotion), `Q4` (engine seam), `Q5` (op-log EDB rows, extension predicate example).

`A10` is unchanged: `ARCHITECTURE.md`'s "Current layout note" still places the comment crates in this repo.

`Q8` (magic-set demand loop) is a MATCHES today but is under an active, undone decision to remove it — `../git-query/DEVPLAN.md` §0 (2026-07-30) cuts the mode system, pass 8, `rewrite.rs`, and `demand.rs` in favor of eager footprint materialization with a two-level cache.
Flagged here so the next audit checks whether that removal has landed.

## Dependency order for closing the gap

1. **`X2` — `git-effect` runtime, in `../git-query`'s repo** — `EffectDoc`, trigger detection, push-intent refs, the executor seam, idempotence keys, the transition log, the `git effect` CLI.
   Per `../git-query/DEVPLAN-effect.md` Part B.
   Unblocks `X5` (`Action` record registration), `F3` (merge gates).
2. **`Q3`, `Q4`, `Q5` in `git-query`** — cache refs and promotion (the door from derived to authoritative, needed before `X2`'s promotion path is meaningful), the Nemo engine seam, and op-log EDB rows once `X6` lands.
3. **`X6` — the op-log** — a store typed doc + query base-fact predicates + anchor's op-log oracle consumer.
   Sharpens `key_valid_at` from its current honest-gap placeholder to real semantics; not otherwise blocking.
4. **`F1` — forge comment storage** — wire `gix-comment` into `add_comment`/`add_anchored_comment`, unblocking `F2` (edit history) and giving `F4` (issues/reviews) a comment substrate to sit beside.
5. **`Q8`'s scheduled removal** — execute `../git-query/DEVPLAN.md` §0 (drop the mode system, magic sets, demand loop; land eager footprint materialization + the two-level cache) and update `ARCHITECTURE.md`'s git-query section to match, since the current text still describes the machinery being removed.
