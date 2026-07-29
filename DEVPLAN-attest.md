# git-attest — dev plan

A cryptographic claim primitive for git: signed, immutable statements binding a **principal** (signer), a **predicate** (content-hashed schema), a **target** (typed reference into git objects), and a **signature**.
Two crates, this repo's established shape: **`gix-attest`** (library) and **`git-attest`** (CLI, invoked as `git attest`).

This is new design, not a port — unlike `gix-anchor`, there is no `../git-ents` source to extract and no `gix-store` precedent for signing.
`gix-anchor`'s ref-store pattern (`refs/anchors/<target-oid>/<id>`, `refname.rs`-style validation, notes-style commits) is the closest existing model and should inform, not dictate, this design.

## Non-negotiable boundaries (do not relitigate)

1. **Cryptographic-only.**
   `verify` answers "did P validly sign C over T" — nothing about whether C is *true* or a predicate is *satisfied*.
   No `attest check`, ever, including as sugar.
   This boundary must be enforced by `verify`'s output *type*, not by convention — see Phase 2 and Testing.
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

## Phase 0 — Target grammar spec (design, no code merged to `main`)

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
Do not proceed to Phase 1 until this holds — the typed target is the parameter every other phase's API takes.

Acceptance: `docs/specification.adoc` merged with target-kind requirement blocks; a standalone `resolve` prototype (may be a scratch binary, doesn't need to be the final CLI) passes the round-trip property test over the fixture set.

## Phase 1 — Ref layout decision

Equal weight to Phase 0 per the design brief — do not let it default to "copy `gix-anchor`'s `refs/anchors/<target-oid>/<id>`" without evidence, since a claim's target can be a *range* or *pair*, which don't have a single oid to key on the way an anchor's single-object target does.

Work:

- Enumerate discovery access patterns the CLI must support: "all claims about this target" (`show`, `verify`), "all claims on a ref" (`log`), "find a claim by id" (`revoke`, `verify <claim-id>`).
  Ref layout must serve all three without a full-repo ref scan for the common case.
- Candidate layouts to evaluate against those patterns (do not silently pick
  one): (a) `gix-anchor`-style `refs/attest/<target-key>/<claim-id>` where
  `<target-key>` is a derived stable hash of the canonical typed target
  (handles ranges/pairs uniformly, since it hashes the *canonical form*, not
  a single oid); (b) a flat `refs/attest/<claim-id>` namespace with an
  index object (commit trailer or tree entry) carrying the target binding,
  scanned or indexed separately; (c) depending on `gix-anchor`'s store
  module directly (as a library dependency, not a copy) if its notes-style
  engine generalizes to non-single-oid targets without contortion.
- Prototype (b) is worth taking seriously even though (a) is the closest
  analogy: unlike an anchor, a claim's identity (the claim-id) is
  independent of its target's oid in a way that matters for `revoke`, which
  needs to find a claim by id, not by target.

**Decision point — ref layout.**
Evidence needed: for each candidate, trace through all three access patterns above and confirm no pattern requires an unbounded ref scan; confirm revocation-chaining (boundary 3) is expressible without deleting or rewriting the revoked claim's ref.
Settle before Phase 3 (storage) begins — Phase 2 (claim model) does not depend on this, only on the *existence* of a `ClaimId` type, so it can proceed in parallel.

Acceptance: a short design note (append to `docs/specification.adoc` or a sibling doc) naming the chosen layout and which access pattern ruled out each rejected candidate.

## Phase 2 — `crates/gix-attest` (claim model + verify)

Can start once Phase 0's target type exists; does not need Phase 1 resolved (uses an opaque `ClaimId`, not yet a ref path).

- Core types: `Predicate` (schema content hash), `Target` (Phase 0's typed
  target), `Principal` (signing identity — reuse whatever key-identity
  representation git's own commit signing uses, do not invent a new one),
  `Claim` (principal + predicate + target + signature, immutable), `ClaimId`.
- `verify(claim, trust_root) -> VerifyOutcome` — **this is the boundary enforcement point.**
  `VerifyOutcome` must be a closed type that can express only: signature valid/invalid, target binding intact/broken, signer resolves under `trust_root` yes/no/unresolvable, and a structural/parse error variant.
  It must have **no variant, field, or string-typed escape hatch** that could carry a predicate-satisfaction verdict — see Testing strategy for how this is checked structurally, not just by review.
- `revoke` as a claim-shaped value: a `Claim` whose predicate is the well-known revocation predicate and whose target is the revoked `ClaimId`.
  Confirm this fits the `Target` type from Phase 0 (a claim-id is not a git object — decide whether `Target` needs a `ClaimId` variant or whether revocation targets are modeled outside `Target` entirely; flag this as a design gap, see Inconsistencies below).
- `key add`/`rotate` as claims whose target is a new public key: confirm the
  same target-type question applies (a public key is not a git object oid
  either).
- Unit tests + proptests over claim construction/signing round-trip, mirroring
  `gix-anchor`'s inline `#[cfg(test)]` + `fixture.rs` convention.

Acceptance: `gix-attest` compiles and tests green with no dependency on Phase 1's ref layout; `VerifyOutcome`'s shape reviewed explicitly against the boundary-violation test in Testing strategy below.

## Phase 3 — Storage

Depends on Phase 1's decision.

- Implement the chosen ref layout.
  If depending on `gix-anchor` as a library (candidate (c) above), take it as a normal crate dependency — do not vendor or fork.
- Signed commits as the storage encoding for claims (per design: "imposes discipline on git's existing signing machinery," not a new object type).
  Confirm how `Claim`'s fields map onto a commit's author/committer/message/ signature fields and where the predicate hash and target encoding live (commit trailer vs. tree entry) — this is a sub-decision of Phase 1, name it explicitly in the ref-layout design note rather than deciding it ad hoc while coding.
- Retention: nothing deleted (boundary 3) — verify this holds structurally
  for the chosen layout (e.g. `revoke` must not be capable of force-updating
  or deleting the revoked claim's ref).

Acceptance: round-trip a claim through storage and back (`sign` writes, `show`/`log` reads) against a scratch repo fixture, using `test-support`'s `init_repo` convention.

## Phase 4 — `crates/git-attest` (CLI)

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

## Phase 5 — DSSE / in-toto interop decision + docs

Deliberately last: the design brief names this a known tension (DSSE-envelope passthrough duplicates signing inside an already-signed commit, vs. treating in-toto as an export/conversion layer over the git-native format) and nothing upstream of this phase depends on resolving it — commit signing (Phase 3) and the CLI (Phase 4) work regardless.

**Decision point — DSSE/in-toto.**
Evidence needed: survey what an actual in-toto consumer downstream (e.g. SLSA tooling, sigstore-adjacent verifiers) needs to accept — do they require a DSSE envelope byte-for-byte, or would a conversion command (`git attest export --format in-toto <claim-id>`) satisfy the interop need without carrying DSSE's own signing layer inside git's?
Recommend defaulting to export-as-conversion (keeps the git-native signed-commit format canonical, avoids double-signing) unless survey evidence shows a specific consumer requires envelope passthrough.
Do not implement either until the survey is written down.

- `docs/specification.adoc`: full requirement-block coverage (`attest.*`
  ids) for claim model, target grammar (carried over from Phase 0), ref
  layout, and the verify/policy boundary.
- README (root + per-crate), feature table if applicable.
- `cargo fmt`/`clippy`/`deny check`/typos/rumdl/committed clean; CI green.

Acceptance: DSSE decision recorded in the spec doc with its evidence, even if the decision is "defer, export-as-conversion is the interim answer."

## Testing strategy

- **Boundary enforcement (cryptographic-only) is a type-level test, not a policy of review discipline.**
  Concretely: a unit test that pattern-matches exhaustively (`match outcome { ... }`, no wildcard arm) over every `VerifyOutcome` variant and asserts each is expressible only in crypto/binding/trust-chain terms.
  Because the match has no wildcard arm, adding a variant that smuggles in predicate-satisfaction semantics (e.g. a hypothetical `PredicateSatisfied(bool)`) fails to compile against the exhaustive test until the test author consciously extends the match — this is the structural catch the design brief asks for, not a comment or a lint.
- Target grammar: property-based round-trip tests from Phase 0, run against
  every phase that touches `Target` (construction, storage encoding, CLI
  parsing) so a canonical-form regression is caught at whichever layer
  introduced it.
- Storage/ref layout: fixture-based integration tests asserting revocation
  never deletes or rewrites a ref (boundary 3) — assert the pre-revocation
  ref still resolves to the same commit oid after `revoke` runs.
- CLI: per-subcommand integration tests per `git-anchor/tests/cli.rs`
  convention, plus an explicit test that `verify`'s exit code distinguishes
  invalid-outcome from could-not-run (Phase 4).
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

1. Target grammar closure (Phase 0) — gates everything.
2. Ref layout (Phase 1) — gates Phase 3; independent of Phase 2.
3. Claim-id / public-key targeting: does `Target` (Phase 0's type) need
   variants for non-git-object targets (a `ClaimId`, a public key), or is
   revocation/key-rotation modeled as a distinct binding outside `Target`
   entirely? (raised in Phase 2 — see Inconsistencies below)
4. Commit-encoding sub-decision of ref layout: predicate hash and target
   encoding as commit trailer vs. tree entry (Phase 3).
5. DSSE/in-toto interop: export-as-conversion vs. envelope passthrough
   (Phase 5) — recommend export-as-conversion pending consumer survey.
6. Whether `gix-attest` depends on `gix-anchor` as a library for ref
   persistence (ref-layout candidate (c)) vs. an independent minimal
   ref-store layer, mirroring `gix-anchor`'s own open decision about
   `gix-store` in `DEVPLAN.md:56`.

## Internal inconsistencies flagged (not silently resolved)

- **Target grammar vs. revocation/key-rotation.**
  The design states targets are "a typed reference into git objects" enumerated as blob/tree/commit/ range/hybrid-pair — all git-object-shaped.
  But `revoke <claim-id>` targets a *claim*, and `key add|rotate`'s "generic case" targets a *public key* — neither is a git object oid.
  Either `Target` must grow non-git-object variants (widening the "typed reference into git objects" definition), or revocation/rotation claims bind through a different mechanism than `Target`, in which case the brief's framing of them as "just a claim whose target is X" is imprecise and the CLI/library boundary between "target" and "generic claim subject" needs its own definition.
  This plan does not resolve it — Phase 2 must decide it, and Phase 0's grammar spec should either accommodate it or explicitly scope it out with a named follow-on.
- **Ref layout vs. range/pair targets.**
  `gix-anchor`'s `refs/anchors/<target-oid>/<id>` layout (the nearest precedent) assumes a single stable oid per target.
  Commit-range and hybrid tree-pair targets have no single oid; Phase 1 must pick a derivation (e.g. hash of the canonical target form) rather than reuse the oid-keyed pattern verbatim.
  Flagged here so the temptation to copy `gix-anchor`'s layout unmodified is caught before it's coded, not after.
