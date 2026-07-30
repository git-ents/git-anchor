# git-attest — dev plan

An envelope primitive for git: immutable, chained claims binding a **principal**, an opaque **target**, and an opaque **payload**.
Two crates, this repo family's established shape: **`gix-attest`** (library) and **`git-attest`** (CLI, invoked as `git attest`).
Per [`ARCHITECTURE.md`](ARCHITECTURE.md), `gix-attest` lives in its own repo, sibling to `gix-anchor` — neither depends on the other.

This is new design, not a port.
The shared storage layers it builds on live in `../git-store`:

- **`gix-refstore`** — trait-based CAS ref persistence (`RefStore`/`Committer`, `RefName`/`RefPath` validation).
- **`gix-store`** — typed kinds/schemas/entities as commit chains, generic over any `RefStore` + object database, including the dynamic (schema-only) read/write path.

`gix-attest` builds on these directly; it gets no storage engine of its own, the same rule `gix-anchor` follows.

## Scope: envelope only

`gix-attest` owns **chaining, revocation, and key lifecycle** — the machinery that makes a sequence of claims immutable, discoverable, and attributable to a principal.
It does not own what a claim is *about*.

- **Target is opaque.** `gix-attest` records a reference and chains claims against it; it does not parse, validate, or enumerate target kinds.
- **Payload is opaque.**
  A claim's content is whatever the caller hands it — a `gix-store` entity reference, typically — and `gix-attest` never interprets its shape.

Concretely: a rebind pin (a human or tool override of a `bind` resolution) and an action-cache record are `gix-store` schemas owned by `git-query` and `git-effect` respectively, registered as ordinary kinds — not attest-native types.
`gix-attest` is the chain those schemas' entities can ride on, not the vocabulary for what rides.
This is the same boundary `gix-anchor` draws around `Binding`: a primitive defines a mechanism, a consumer defines a domain shape, and the two are different crates.

## Deferred, per `ARCHITECTURE.md`

Cryptographic signing is out of scope for now: authority is a ref transition in a repository you trust, and there is no `Signer` seam in `gix-store`.
A claim is a **chained commit with an empty signature sentinel** — `facet-git-tree` writes every field present, so the sentinel costs nothing to fill in once signing lands.
DSSE/in-toto interop is likewise deferred to one sentence: it is an export/conversion question for whenever a concrete consumer needs it, not a design constraint today.

## Claim shape

- `ClaimId` — the claim's own commit oid.
- `Principal` — the writing identity, reused from whatever key-identity representation git's own commit signing uses; not verified today, just recorded.
- `Target` — opaque; whatever bytes the caller supplies, chained against.
- `Payload` — opaque; typically a reference to a `gix-store` entity the caller owns the schema for.
- `prev` — the chain link: `None` for a genesis claim, the prior `ClaimId` otherwise.
- `signature` — the sentinel field above.

`revoke` is a claim-shaped value whose `Target` is the revoked `ClaimId` and whose `Payload` is the well-known revocation marker.
Key rotation is the same shape, targeting a prior key claim.
Neither needs a `Target` variant beyond "opaque bytes" — a `ClaimId` and a public key both serialize to bytes just as any other target does; the earlier draft of this plan treated that as an open typing question and it was not one.

Nothing is ever deleted: revocation and rotation are claims chained onto what they revoke or rotate, never edits or ref deletes.

## Storage

Claims are `gix-store` entities, one global kind, nested under `gix-store`'s `RefPath` the same way `gix-anchor` groups notes: `<target-hash>/<claim-id>`, where `target-hash` is a content hash of the opaque `Target` bytes — a derivation that works uniformly whether `Target` names a single object or something with no single oid, because it hashes the opaque encoding, not a semantic interpretation of it.
No bespoke ref-CAS layer: `gix-refstore`/`gix-store` do the writing, exactly as `ARCHITECTURE.md` requires.
Claim writes are create-only (`RefEdit::Create`, never an update) — a claim's ref history is always exactly one commit, since revocation is a new claim, not a rewrite of the old one.

## Phase 1 — Claim model

Core types (`ClaimId`, `Principal`, `Target`, `Payload`, `Claim`) and the chain: construction, `revoke`, key add/rotate, all opaque-target/opaque-payload per the scope above.
Unit tests + proptests over construction and chain round-trip, mirroring `gix-anchor`'s inline `#[cfg(test)]` + `fixture.rs` convention.

Acceptance: `gix-attest` compiles and tests green with no dependency on a concrete `Target`/`Payload` shape — construct claims in tests from arbitrary byte strings and confirm the chain and revocation logic don't care what they mean.

## Phase 2 — Storage

Round-trip a claim through `gix-store`: write, read by id, read by target-hash prefix, walk a chain.
Confirm structurally that revocation cannot force-update or delete the revoked claim's ref — a fixture-based test, not a review note.

Acceptance: `sign`/`show`/`log`-equivalent round-trip against a scratch repo fixture, using `test-support`'s `init_repo` convention.

## Phase 3 — CLI

Mirror `git-anchor`'s CLI shape (`clap` derive, `anyhow`, thin `main.rs`).

Write: `claim <target> <payload>`, `revoke <claim-id>`, `key add|rotate`.
Read: `show <target>`, `log [<target>]`, `verify <claim-id>`.

`verify` today can only ever report the chain-structural outcomes — links resolve, no gap, no cycle — plus a signature outcome fixed at "unsigned (deferred)."
Its output type must still be closed and exhaustively matched, so that the day signing lands, adding a real signature outcome is a compile-time-forced change to every match site, not a silent widening.

Acceptance: `git attest claim/revoke/key/show/log/verify` round-trip against a scratch repo; `cargo test --workspace` green including the new crates.

## Open questions, not solved here

- **Target-syntax canonicalization.**
  `gix-attest` treats a target as opaque bytes, but *something* upstream of it has to turn "this line range," "this commit," "this action" into a canonical byte encoding before it's stable to hash and chain against.
  `git-effect`'s action-cache-key derivation is the same problem restated: a canonical, stable encoding for "the thing a rule fired on."
  Solve it once, shared, when a concrete consumer forces the question — not twice, once per crate that happens to need it first.
- **DSSE/in-toto interop.**
  Export-as-conversion (`git attest export --format in-toto <claim-id>`) is the likely answer once a concrete consumer needs it; no design work until then.

## Definition of done

- `cargo test --workspace` passes, doctests included, for the two new crates.
- `git attest claim/revoke/key/show/log/verify` round-trip against a scratch repo.
- No `attest check` or policy-evaluating verb exists anywhere in the CLI or library public API — `verify` answers structural-chain and (deferred) signature questions only, never predicate satisfaction.
- No `Target`/`Payload` variant, field, or special case exists for a specific consumer's vocabulary (a rebind pin, an action record) — those stay `gix-store` schemas owned elsewhere.
