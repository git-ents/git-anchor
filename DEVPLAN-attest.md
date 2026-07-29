# git-attest — dev plan

A cryptographic claim primitive for git: signed, immutable statements binding a **principal** (signer), a **predicate** (content-hashed schema), a **target** (typed reference into git objects), and a **signature**.
Two crates, this repo's established shape: **`gix-attest`** (library) and **`git-attest`** (CLI, invoked as `git attest`).

This is new design, not a port — unlike `gix-anchor`, there is no `../git-ents` source to extract and no `gix-store` precedent for signing.
`gix-anchor`'s ref-store pattern (`refs/anchors/<target-oid>/<id>`, notes-style commits) is the closest existing model, and since Phase 0 it is also the shared *engine*: `gix-anchor` now stores through `gix-refstore` rather than its own copy of it.
The shared storage layers live in `../git-store` as two crates:

- **`gix-refstore`** — trait-based CAS ref persistence (`RefStore`/`Committer`, `RefName`/`RefSegment` validation, `GixRefStore` over a real repo, `MemoryRefStore` for tests).
- **`gix-store`** — typed kinds/schemas/entities as commit chains (`{value/, schema/}` trees, `Schema:` trailer provenance), generic over any `RefStore` + object database.

`gix-attest` should build on these rather than growing a third copy of the ref-CAS machinery (see Phase 2 candidate (c) and Phase 4).
One known gap to resolve upstream: `gix-store`'s commit writer emits unsigned commits (`extra_headers: Vec::new()` in `store.rs`), and signed commits are this design's canonical claim encoding — see Phase 4.

## Non-negotiable boundaries (do not relitigate)

1. **Cryptographic-only.**
   `verify` answers "did P validly sign C over T" — nothing about whether C is *true* or a predicate is *satisfied*.
   No `attest check`, ever, including as sugar.
   This boundary must be enforced by `verify`'s output *type*, not by convention — see Phase 3 and Testing.
2. **Trust roots are inputs**, never discovered/configured by the tool.
   `verify` takes a trust root argument (e.g. a ref); no trusted-keys config.
3. **Nothing is deleted.**
   Revocation is a claim chained onto what it revokes.
4. **No parallel grammar.**
   Target syntax canonicalizes gitrevisions; `resolve` is the sole producer of canonical typed target forms.
5. Correctness over speed; small surface over convenience.
   When in doubt, leave it out.

## Why this plan is phased the way it is

Two decisions gate almost everything downstream and cannot be made by building — they need evidence:

- **Target grammar** (what a target *is*, in the type system, before any
  storage or CLI code touches it).
- **Ref layout** (how a claim is discovered from a target — an encoding
  choice with format-migration cost if wrong).

Everything else (signing, storage, CLI verbs) is comparatively mechanical once these are fixed.
The plan therefore front-loads a design-and-prototype phase for both before committing to library structure, rather than discovering their shape while implementing Phase 3 of `gix-anchor`-style work.

Ahead of both sits Phase 0, the one piece of *code* that precedes the design work: unifying the storage engine `gix-attest` will build on.
It is first because it is cheapest first — `gix-anchor` already has the test suite to prove the swap correct, and doing it later would mean migrating two consumers instead of one.
Phases 1 and 2 are design work and can proceed in parallel with it if the people differ.

## Phase 0 — Migrate `gix-anchor` onto `gix-refstore` (prerequisite)

Do this **first**, before any `gix-attest` design work.
Rationale: `gix-attest` must not be the second consumer of a storage engine that exists in two copies.
Migrating `gix-anchor` first means there is exactly one ref-CAS engine in the family when `gix-attest` arrives, and it means the engine gets exercised and hardened by a real consumer (with a full existing test suite) before a security-sensitive crate depends on it.
This was open decision 7 in an earlier draft of this plan; it is promoted to blocking work.

### Feasibility, already confirmed

- `../git-store` local `main` is `345901c`, identical to its pushed remote — so `gix-refstore` is reachable as a git dependency exactly the way `facet-git-tree` already is (`{ git = "https://github.com/git-ents/git-store" }`).
- `RefName` accepts multi-segment names (`refs/anchors/<target-hex>/<id-hex>` is valid), and `RefPrefix::prefixed` treats the boundary as a whole segment, which is what `Store::list`/`refs_under` needs.
- `RefEdit` has a `Delete` variant, so `Store::remove` survives the migration unchanged.
- `GixRefStore` already implements `Committer` off `repo.committer()`, and its per-ref lock (`gix-refstore-locks/`, 30s backoff, `%`/`/` percent-escaping) is the same scheme `gix-anchor/src/store.rs` hand-rolled — so this is a genuine de-duplication, not a re-architecture.

### Scope

Rewrite `crates/gix-anchor/src/store.rs` (684 lines) against `gix-refstore`, and delete `crates/gix-anchor/src/refname.rs` (38 lines) outright — `RefName`/`RefSegment` validation supersedes `check_hex_component`.
Mirror `gix-store`'s own specialization pattern so the common call stays a one-liner:

```rust
pub type RepoStore<'r> = Store<GixRefStore<'r>, &'r gix::OdbHandle>;
// Store<R, O> where R: RefStore + Committer, O: Find + Write
```

Everything above `store.rs` — `anchor.rs`, `binding.rs`, `diff.rs`, `projection.rs` (~3.5k lines, the actual anchor semantics) — is untouched.
This is a storage-layer swap, not a rewrite of the crate.

### Stages (each independently reviewable and committable)

**Done, 2026-07-29.**
Shipped in four commits rather than the six stages planned below, because stages 1–5 could not be separated without a transitional `Store` holding both a `&Repository` and a ref backend — two sources of truth for the same state, which the type system should forbid rather than host.
What landed: `923fd1c` types the ref layout (a private `NoteRef { target, id }` whose construction is infallible, since an `ObjectId`'s hex is always a valid `RefSegment` — `refname.rs` and `Error::InvalidRefComponent` deleted), `3d83edd` swaps the engine, `d338351` adds the `MemoryRefStore` tests, `2e7c52d` fixes an author-identity regression the review caught.

Three things the plan below did not anticipate:

- **The reflog watch item was a non-issue.**
  Anchor refs never had a reflog: git enables them by default only under `refs/heads`, `refs/remotes`, `refs/notes` and `HEAD`.
  Verified empirically on both sides of the change rather than reasoned about.
- **Removing the lock widened the retry loop, not just the ref edit.**
  The old code held its lock across read-tip → read `created_at` → commit.
  With CAS, everything derived from the tip has to be re-read per attempt, or a note appearing mid-flight gets a fresh `created_at` instead of the existing one's — a silent data bug no existing test could have caught.
- **`create` cannot retry a fixed edit.**
  Its ref name derives from the genesis commit's own oid, and `apply` reports both a taken identity and transient lock contention as `LostRace`.
  It distinguishes them by reading the ref: present means taken (`Error::GenesisExists`), absent means retry.

The stage list as originally planned:

1. **Dependency + `RepoStore` skeleton.**
   Add `gix-refstore` as a git dep; introduce `Store<R, O>` generic over `RefStore + Committer` / `Find + Write` with the `RepoStore<'r>` alias and `Store::open`/`with_prefix` preserved as constructors over it.
   No behavior change yet — the internals may still route through the old code paths.
2. **Ref plumbing.**
   Replace `anchor_ref`/`refs_under`/`find_ref`/`tip` with `RefName`/`RefPrefix` construction and `RefStore::read`/`prefixed`; delete `refname.rs` and fold `Error::InvalidRefComponent` onto `InvalidRefName`.
3. **Write path.**
   Replace `commit_forward`'s hand-rolled `lock_ref` + `MAX_CAS_ATTEMPTS` retry with `RefEdit` + `RefStore::apply`'s `LostRace` loop, and replace `repo.commit`/`repo.new_commit` with a `write_commit` taking its signature from `Committer`.
   Delete `LOCK_DIR`/`LOCK_TIMEOUT`/`encode_ref`/`is_retryable` — all now `gix-refstore`'s job.
   **Watch item:** `repo.commit()` writes a reflog entry; a raw object write plus `RefEdit` may not.
   Decide deliberately whether anchor refs keep reflogs, and say so in the commit message rather than letting it change silently.
4. **Error surface.**
   Collapse `Error::CasExhausted` (no longer reachable — `apply` retries rather than exhausting) and `Error::InvalidRefComponent`, adding a `gix-refstore`-backed variant.
   This is the only *public* API break in the migration; it is a pre-1.0 crate, so take it cleanly rather than papering over it.
5. **Consumer fixups.**
   `gix-comment/src/comment.rs:140` holds `store: Store<'r>` → `RepoStore<'r>`; `git-anchor/src/main.rs:172` calls `Store::open(&repo)` (should need no change if stage 1 preserved it).
   `gix-comment-lsp` reaches storage only through `gix-comment`, so it should need nothing.
6. **Test migration + the new seam.**
   `crates/gix-anchor/tests/store.rs` (417 lines) must stay green *unchanged* wherever it exercises behavior rather than construction — that suite is the migration's correctness oracle.
   Then add what was previously impossible: `MemoryRefStore`-backed unit tests for layout/CAS logic with no tempdir repo.

### Acceptance

All met.

- `cargo test --workspace` green, doctests included; `tests/store.rs` is byte-for-byte unmodified, not merely behaviorally unmodified.
- `git anchor add/list/show/project/remove` still round-trips against a scratch repo.
- No per-ref locking, CAS retry, or refname validation code remains in `crates/gix-anchor/`.
- `DEVPLAN.md:56`'s recommendation is finally true, and `DEVPLAN.md` says so rather than posing it as an open decision.

One acceptance criterion was added in flight: the CAS paths must be *covered*, not merely working.
The pre-existing suite needed a tempdir repo and ran sequentially, so it could not reach a lost race at all — the migration's own oracle was blind to the part that changed most.
`d338351` closes that with a fault-injecting `RefStore` decorator that lands a real conflicting edit before the caller's, so the lost race comes from the backend's own precondition check rather than a fabricated error.
Each retry test was checked to fail when its loop is reverted.

**Substrate change this phase forced.**
`Committer` yielded one signature, used as both author and committer, which silently overwrote authorship for any repository setting `author.*`/`GIT_AUTHOR_*` apart from the committer — a regression against `gix::Repository::commit`.
Fixed upstream in git-store (`4581dcf`) as a `Committer::author` provided method defaulting to `signature`, so no implementor breaks and `gix-store` is corrected too.
This is the first evidence for the Phase 4 sub-decision below: the `Committer`/`Signer` boundary is where identity and signing belong, and extending it upstream works without breaking existing backends.

### Explicitly not in this phase

Adopting `gix-store` proper (kinds/schemas/entities) for anchors.
`gix-anchor`'s `Note` document is its own shape and does not need `gix-store`'s `{value/, schema/}` entity model; only the ref layer is shared.
Revisit only if a concrete need appears.
Note also that `../git-store` recently renamed `Schema` → `Node` and `SchemaDoc` → `Schema` (`052bf8a`), so any later `gix-store` adoption should read the current names off the source rather than trusting this plan's vocabulary.

## Phase 1 — Target grammar spec (design, no code merged to `main`)

Target syntax is the hard problem: it becomes shared vocabulary consumed by `git-query`/`git-forge`/CI gates later, so its cost of being wrong is paid by every downstream consumer, not just this crate.

Work:

- Enumerate the closed set of target *kinds* from the design: blob, blob-at-path@rev, tree/subtree, commit, commit range, hybrid (base, tip) tree pair.
  For each, define the canonical typed form as a Rust type (sum type over kinds) — not yet the storage/CLI encoding, just the value the rest of the library will pass around.
- Define the gitrevisions subset each kind accepts as input to `resolve` (e.g. commit ranges accept `A..B`; blob-at-path needs `<rev>:<path>` disambiguation from a bare blob oid).
  Explicitly enumerate gitrevisions syntax that is *out of scope* (e.g. reflog shorthand, `@{upstream}`) rather than silently accepting or rejecting it at parse time.
- Write the spec as `docs/specification.adoc` requirement blocks (this
  repo's convention, `anchor.*`-id style) *before* writing `resolve`'s
  parser, so the parser has a spec to be tested against rather than the spec
  being reverse-engineered from the implementation.
- Prototype `resolve <target-spec>` against `gix::revision` parsing (already
  a `gix-anchor` dependency) on a scratch repo with fixtures covering every
  kind, including adversarial ones: a path that doesn't exist at the given
  rev, a commit range where the base is not an ancestor of the tip, a blob
  oid that collides syntactically with a short commit oid.

**Decision point — target grammar closure.**
Evidence needed: does every kind round-trip through `resolve` → canonical form → back to a gitrevisions string a human would recognize?
Settle by writing the round-trip property test *first* (even as a throwaway harness) and only finalizing the type once it's green over the fixture set.
Do not proceed to Phase 2 until this holds — the typed target is the parameter every other phase's API takes.

Acceptance: `docs/specification.adoc` merged with target-kind requirement blocks; a standalone `resolve` prototype (may be a scratch binary, doesn't need to be the final CLI) passes the round-trip property test over the fixture set.

## Phase 2 — Ref layout decision

Equal weight to Phase 1 per the design brief — do not let it default to "copy `gix-anchor`'s `refs/anchors/<target-oid>/<id>`" without evidence, since a claim's target can be a *range* or *pair*, which don't have a single oid to key on the way an anchor's single-object target does.

Work:

- Enumerate discovery access patterns the CLI must support: "all claims about this target" (`show`, `verify`), "all claims on a ref" (`log`), "find a claim by id" (`revoke`, `verify <claim-id>`).
  Ref layout must serve all three without a full-repo ref scan for the common case.
- Candidate layouts to evaluate against those patterns (do not silently pick
  one): (a) `gix-anchor`-style `refs/attest/<target-key>/<claim-id>` where
  `<target-key>` is a derived stable hash of the canonical typed target
  (handles ranges/pairs uniformly, since it hashes the *canonical form*, not
  a single oid); (b) a flat `refs/attest/<claim-id>` namespace with an
  index object (commit trailer or tree entry) carrying the target binding,
  scanned or indexed separately; (c) `gix-store`'s own
  `refs/store/<kind>/<name>` layout, if claims are stored as a `gix-store`
  kind (see Phase 4) — its `<name>` is a single `RefSegment`, so target-key
  grouping would live in an index or in the segment derivation, not in ref
  nesting.
- Whichever layout wins, ref *persistence* is `gix-refstore` (git dep on `../git-store`, same as `facet-git-tree` today): `RefName`/`RefSegment` validation replaces a local `refname.rs`, and `RefStore::apply`'s CAS replaces a hand-rolled lock/retry loop.
  Do **not** depend on `gix-anchor`'s `store.rs` for this — it is anchor-semantic (`Binding`-keyed identities, a `remove()` that deletes refs, directly at odds with boundary 3) and is itself a local duplicate of what `gix-refstore` now provides.
  The layout decision is thereby decoupled from the engine decision: (a)/(b)/(c) differ only in ref naming and discovery, not in who does the CAS.
- Prototype (b) is worth taking seriously even though (a) is the closest
  analogy: unlike an anchor, a claim's identity (the claim-id) is
  independent of its target's oid in a way that matters for `revoke`, which
  needs to find a claim by id, not by target.

**Decision point — ref layout.**
Evidence needed: for each candidate, trace through all three access patterns above and confirm no pattern requires an unbounded ref scan; confirm revocation-chaining (boundary 3) is expressible without deleting or rewriting the revoked claim's ref.
Settle before Phase 4 (storage) begins — Phase 3 (claim model) does not depend on this, only on the *existence* of a `ClaimId` type, so it can proceed in parallel.

Acceptance: a short design note (append to `docs/specification.adoc` or a sibling doc) naming the chosen layout and which access pattern ruled out each rejected candidate.

## Phase 3 — `crates/gix-attest` (claim model + verify)

Can start once Phase 1's target type exists; does not need Phase 2 resolved (uses an opaque `ClaimId`, not yet a ref path).

- Core types: `Predicate` (schema content hash), `Target` (Phase 1's typed
  target), `Principal` (signing identity — reuse whatever key-identity
  representation git's own commit signing uses, do not invent a new one),
  `Claim` (principal + predicate + target + signature, immutable), `ClaimId`.
- `Predicate` should be evaluated against `gix-store`'s schema model before inventing its own hashing: a `gix-store` kind is already a *published, content-addressed schema* (`refs/schema/<kind>`, `schema/` subtree, `Schema:` trailer provenance carried by every entity).
  If a predicate *is* a published `facet-git-tree` `Schema`, its content hash is the schema tree's own oid — free, canonical, and shared vocabulary with every other `gix-store` consumer.
  Only diverge if a predicate turns out not to be schema-shaped (record why in the spec doc if so).
- `verify(claim, trust_root) -> VerifyOutcome` — **this is the boundary enforcement point.**
  `VerifyOutcome` must be a closed type that can express only: signature valid/invalid, target binding intact/broken, signer resolves under `trust_root` yes/no/unresolvable, and a structural/parse error variant.
  It must have **no variant, field, or string-typed escape hatch** that could carry a predicate-satisfaction verdict — see Testing strategy for how this is checked structurally, not just by review.
- `revoke` as a claim-shaped value: a `Claim` whose predicate is the well-known revocation predicate and whose target is the revoked `ClaimId`.
  Confirm this fits the `Target` type from Phase 1 (a claim-id is not a git object — decide whether `Target` needs a `ClaimId` variant or whether revocation targets are modeled outside `Target` entirely; flag this as a design gap, see Inconsistencies below).
- `key add`/`rotate` as claims whose target is a new public key: confirm the
  same target-type question applies (a public key is not a git object oid
  either).
- Unit tests + proptests over claim construction/signing round-trip, mirroring
  `gix-anchor`'s inline `#[cfg(test)]` + `fixture.rs` convention.

Acceptance: `gix-attest` compiles and tests green with no dependency on Phase 2's ref layout; `VerifyOutcome`'s shape reviewed explicitly against the boundary-violation test in Testing strategy below.

## Phase 4 — Storage

Depends on Phase 2's decision.

- Storage layering (bottom-up): `gix-refstore` for ref persistence (non-negotiable — see Phase 2), and `gix-store`'s kind/schema engine on top of it *if* the claim document fits its entity model.
  Both are normal git deps on `../git-store`, like `facet-git-tree` today — do not vendor, fork, or copy either, and do not route storage through `gix-anchor`'s `store.rs`.
- **Sub-decision — signing hook (blocks full `gix-store` reuse).**
  Signed commits are the storage encoding for claims (per design: "imposes discipline on git's existing signing machinery," not a new object type), but `gix-store`'s `write_commit` currently emits `extra_headers: Vec::new()` — no `gpgsig`, so it cannot write a claim commit today.
  Two ways through, decide with the `../git-store` maintainer hat on: (i) extend `gix-store` upstream with a signing hook (a `Signer` counterpart to the existing `Committer` trait, threading a `gpgsig` extra-header through `write_commit`) and store claims as a `gix-store` kind — preferred, since it keeps one commit-writing engine and gives every future `gix-store` consumer signing for free; (ii) interim fallback: use `gix-refstore` directly and write signed claim commits inside `gix-attest` — acceptable to unblock this repo, but record it as debt that converges on (i).
  Either way `verify` reads the signature off the raw commit object, so the read path is identical under both.
- Confirm how `Claim`'s fields map onto a commit's author/committer/message/signature fields and where the predicate hash and target encoding live (commit trailer vs. tree entry) — this is a sub-decision of Phase 2, name it explicitly in the ref-layout design note rather than deciding it ad hoc while coding.
  Note `gix-store` already answers part of this if claims are entities: predicate-as-schema lands in the `schema/` subtree + `Schema:` trailer (Phase 3), leaving only the target encoding to place.
- Retention: nothing deleted (boundary 3) — verify this holds structurally for the chosen layout (e.g. `revoke` must not be capable of force-updating or deleting the revoked claim's ref).
  Both upstream engines expose deletion (`gix-store`'s `Kind::remove`, `RefEdit`-level deletes) — `gix-attest`'s public API must not re-export or wrap any of it, and claim writes should use a create-only expectation (`MustNotExist`-shaped `RefEdit::Create`), never an update, since a claim is immutable — unlike anchors/notes, a claim ref's history is always exactly one commit.

Acceptance: round-trip a claim through storage and back (`sign` writes, `show`/`log` reads) against a scratch repo fixture, using `test-support`'s `init_repo` convention.

## Phase 5 — `crates/git-attest` (CLI)

Mirror `git-anchor`'s CLI shape (`clap` derive, `anyhow`, thin `main.rs`, `gix::discover(".")` + `gix::interrupt`).

Write: `sign <predicate> <target>`, `revoke <claim-id>`, `key add|rotate`.
Read: `show <target>`, `log [<ref>]`, `verify <claim-id|target>`.
Plumbing: `resolve <target-spec>`.

- `verify`'s CLI output must mirror `VerifyOutcome`'s closed shape exactly —
  no CLI-layer text like "claim is valid" without qualifying *what* was
  checked (signature/binding/trust-chain), so a user cannot read policy
  satisfaction into cryptographic validity from the UX either.
- Exit codes: reserve a distinct code for "verify ran but outcome is
  invalid" vs. "verify could not run" (structural/parse error) — these must
  not collapse to the same code, since a caller scripting against this in a
  gate needs to distinguish "signature bad" from "target spec malformed."
- Integration tests per subcommand with `test-support` + `tempfile`,
  following `git-anchor/tests/cli.rs`.

Acceptance: `git attest sign/verify/revoke/show/log/resolve/key` round-trip against a scratch repo; `cargo test --workspace` green including the new crates.

## Phase 6 — DSSE / in-toto interop decision + docs

Deliberately last: the design brief names this a known tension (DSSE-envelope passthrough duplicates signing inside an already-signed commit, vs. treating in-toto as an export/conversion layer over the git-native format) and nothing upstream of this phase depends on resolving it — commit signing (Phase 4) and the CLI (Phase 5) work regardless.

**Decision point — DSSE/in-toto.**
Evidence needed: survey what an actual in-toto consumer downstream (e.g. SLSA tooling, sigstore-adjacent verifiers) needs to accept — do they require a DSSE envelope byte-for-byte, or would a conversion command (`git attest export --format in-toto <claim-id>`) satisfy the interop need without carrying DSSE's own signing layer inside git's?
Recommend defaulting to export-as-conversion (keeps the git-native signed-commit format canonical, avoids double-signing) unless survey evidence shows a specific consumer requires envelope passthrough.
Do not implement either until the survey is written down.

- `docs/specification.adoc`: full requirement-block coverage (`attest.*`
  ids) for claim model, target grammar (carried over from Phase 1), ref
  layout, and the verify/policy boundary.
- README (root + per-crate), feature table if applicable.
- `cargo fmt`/`clippy`/`deny check`/typos/rumdl/committed clean; CI green.

Acceptance: DSSE decision recorded in the spec doc with its evidence, even if the decision is "defer, export-as-conversion is the interim answer."

## Testing strategy

- **Boundary enforcement (cryptographic-only) is a type-level test, not a policy of review discipline.**
  Concretely: a unit test that pattern-matches exhaustively (`match outcome { ... }`, no wildcard arm) over every `VerifyOutcome` variant and asserts each is expressible only in crypto/binding/trust-chain terms.
  Because the match has no wildcard arm, adding a variant that smuggles in predicate-satisfaction semantics (e.g. a hypothetical `PredicateSatisfied(bool)`) fails to compile against the exhaustive test until the test author consciously extends the match — this is the structural catch the design brief asks for, not a comment or a lint.
- Target grammar: property-based round-trip tests from Phase 1, run against
  every phase that touches `Target` (construction, storage encoding, CLI
  parsing) so a canonical-form regression is caught at whichever layer
  introduced it.
- Storage/ref layout: fixture-based integration tests asserting revocation
  never deletes or rewrites a ref (boundary 3) — assert the pre-revocation
  ref still resolves to the same commit oid after `revoke` runs.
- Storage unit tests can run against `gix-refstore`'s `MemoryRefStore`
  (no tempdir repo needed) for layout/CAS/immutability logic, with real-repo
  fixtures reserved for the signing path and CLI integration — a testing
  seam `gix-anchor`'s welded-to-`gix::Repository` store never had.
- CLI: per-subcommand integration tests per `git-anchor/tests/cli.rs`
  convention, plus an explicit test that `verify`'s exit code distinguishes
  invalid-outcome from could-not-run (Phase 5).
- Unit tests + `fixture.rs`-style helpers inline per `gix-anchor` convention;
  `test-support::init_repo` reused for CLI integration tests.

## Definition of done

- `cargo test --workspace` passes, doctests included, for the two new
  crates alongside existing ones.
- `git attest sign/verify/revoke/show/log/resolve/key` round-trip against a
  scratch repo.
- `docs/specification.adoc` has full `attest.*` requirement-block coverage.
- The exhaustive-match `VerifyOutcome` boundary test exists and is named
  clearly enough that a future PR adding a policy-flavored variant trips it.
- No `attest check` or policy-evaluating verb exists anywhere in the CLI or
  library public API.

## Open decisions (in dependency order)

0. ~~Whether to migrate `gix-anchor` onto `gix-refstore`~~ — **settled: yes, and it is now Phase 0**, blocking the rest.
   Kept in this list rather than deleted so the reversal is legible: an earlier draft had it as out-of-scope decision 7.
1. Target grammar closure (Phase 1) — gates every *design* decision below, but Phase 0 lands first as code.
2. Ref layout (Phase 2) — gates Phase 4; independent of Phase 3.
3. Claim-id / public-key targeting: does `Target` (Phase 1's type) need variants for non-git-object targets (a `ClaimId`, a public key), or is revocation/key-rotation modeled as a distinct binding outside `Target` entirely?
   (raised in Phase 3 — see Inconsistencies below)
4. Commit-encoding sub-decision of ref layout: predicate hash and target
   encoding as commit trailer vs. tree entry (Phase 4).
5. DSSE/in-toto interop: export-as-conversion vs. envelope passthrough
   (Phase 6) — recommend export-as-conversion pending consumer survey.
6. How far up the `../git-store` stack to build: `gix-refstore` for ref
   persistence is settled (see Phase 2 — not open), but whether claims are
   full `gix-store` entities hinges on the signing-hook sub-decision
   (Phase 4): extend `gix-store` upstream with a `Signer` hook (preferred)
   vs. interim signed-commit writing over bare `gix-refstore`.
7. Whether `gix-anchor`'s ref history keeps reflog entries after Phase 0's
   write-path swap (`repo.commit` wrote them; a raw object write plus
   `RefEdit` may not) — a behavior question Phase 0 stage 3 must answer
   deliberately rather than discover.

## Internal inconsistencies flagged (not silently resolved)

- **Target grammar vs. revocation/key-rotation.**
  The design states targets are "a typed reference into git objects" enumerated as blob/tree/commit/ range/hybrid-pair — all git-object-shaped.
  But `revoke <claim-id>` targets a *claim*, and `key add|rotate`'s "generic case" targets a *public key* — neither is a git object oid.
  Either `Target` must grow non-git-object variants (widening the "typed reference into git objects" definition), or revocation/rotation claims bind through a different mechanism than `Target`, in which case the brief's framing of them as "just a claim whose target is X" is imprecise and the CLI/library boundary between "target" and "generic claim subject" needs its own definition.
  This plan does not resolve it — Phase 3 must decide it, and Phase 1's grammar spec should either accommodate it or explicitly scope it out with a named follow-on.
- **Ref layout vs. range/pair targets.**
  `gix-anchor`'s `refs/anchors/<target-oid>/<id>` layout (the nearest precedent) assumes a single stable oid per target.
  Commit-range and hybrid tree-pair targets have no single oid; Phase 2 must pick a derivation (e.g. hash of the canonical target form) rather than reuse the oid-keyed pattern verbatim.
  Flagged here so the temptation to copy `gix-anchor`'s layout unmodified is caught before it's coded, not after.
- **`gix-anchor` never took the `gix-store` dependency it recommended.**
  `DEVPLAN.md:56` recommends depending on `gix-store` for ref persistence; the shipped `gix-anchor` instead reimplemented the layer locally (`store.rs`/`refname.rs` — per-ref locks, CAS retry, refname validation), taking only `facet-git-tree` from `../git-store`.
  `../git-store` has since factored exactly that layer out as `gix-refstore`, so the code `gix-anchor` duplicated now exists as a reusable crate.
  This plan's earlier draft pointed ref-layout candidate (c) at `gix-anchor`'s store as if it were the shared engine — it is not; the shared engine is `gix-refstore`/`gix-store`, and this plan now targets those directly.
  **Resolved, not merely flagged:** Phase 0 migrates `gix-anchor` onto `gix-refstore` before any `gix-attest` work begins, so this inconsistency is closed by the plan rather than carried by it.
