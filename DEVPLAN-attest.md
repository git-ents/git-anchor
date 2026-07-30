# git-attest — dev plan

Implement `ARCHITECTURE.md`'s git-attest: signed claim envelopes and chains.
Two crates in this repo's workspace — **`gix-attest`** (library) and **`git-attest`** (CLI, invoked as `git attest`) — added to `crates/` beside `gix-anchor`/`git-anchor`, exactly as git-effect lives in git-query's repo.

Template: `../git-query/DEVPLAN.md` for the document, this repo and `../git-store` for conventions, configs, and workflows.
Naming, non-negotiable: `gix-attest` library, `git-attest` binary, subcommands `sign`, `revoke`, `verify`, `log`, `resolve` — nothing else.

**Colocation is not coupling.**
`ARCHITECTURE.md`: "anchor and attest are siblings.
Neither knows the other exists."
Sharing this workspace changes nothing: `gix-attest` must not depend on `gix-anchor` and `gix-anchor` must not depend on `gix-attest`, ever, and a CI check over `cargo metadata` enforces it the day the crate lands.
The only composition point is git-query, which reads both.
The repo split (DELTA X1 imagined `../git-attest`) can happen later by moving two directories; nothing in this plan makes that harder.

**What is actually being built, stated once:** a claim is a chained commit on a claim ref.
The envelope — target descriptor, payload tree hash, signing-key id — is a store typed doc; the signature bytes ride the claim commit in the standard `gpgsig` header, so `git log --show-signature` verifies claims with stock git; verification is cryptography and nothing else.
Attest owns the envelope and the chain.
It owns no payload, no policy, no validity, no hash function, and no ref-advance rules.

---

## Verdict

Everything attest needs from store exists and is shaped correctly; this is an assembly job, not a substrate job.

- Signing: `gix-refstore`'s `Signer`/`SignatureBytes`/`ErasedSigner` seam, wired into `gix-store`'s `write_commit`, which serializes the commit without the signature header and signs those bytes (`gix-store/src/store.rs:311-337`).
  This is git's own object-signing discipline — and the header must be git's too.
  **Store-side prerequisite (one small change): store the signature as an armored block in the `gpgsig` header, not hex in a bespoke `signature` extra header.**
  Git uses `gpgsig` for every signature format (with `gpg.format = ssh` it holds the ASCII-armored `SSH SIGNATURE` block that `ssh-keygen -Y sign` emits), so `git log --show-signature` and `git verify-commit` work on claim commits with no attest tooling installed.
  The signed payload is unchanged (commit content minus the `gpgsig` header itself — git's rule and store's existing rule coincide); only the storage location and encoding (armored, multi-line continuation) change.
  `Signer` implementations must therefore emit format-armored blocks, not raw signature bytes; the seam stays byte-opaque.
  Attest does not build a signing path; it builds the *reading* half — `Store::signature(commit)` recovers the block.
- Hashing: target keys are `facet_git_tree::normal_form::hash` over a closed-universe descriptor — the same move as anchor ids and action keys, per the doc's "no separate key crates."
- Chaining: `commit_forward`'s CAS-on-expected-tip *is* chain integrity — a claim ref that only ever advances by one commit whose parent is the expected tip cannot fork silently.
- Schema registration with the identity-universe check (`KindSchema::write` → `check_identity_subtrees`) enforces that the target descriptor stays inside the normal form.

The one thing to *replace* rather than build: git-query's interim trailer claim store (`../git-query/crates/gix-query-host/src/claim.rs`), which marks its own replacement seam — "once `git-attest` exists, this is the one place that needs to start asking it to verify the claim before trusting its trailers."
That migration is query-side work, planned in `../git-query/DEVPLAN-effect.md` Part A; this plan only defines the API it lands on.

---

## Decisions

### 1. The envelope is a store typed doc, and store's write path is the only write path

```rust
#[derive(Facet)]
pub struct Envelope {
    pub target: Target,        // normal-form descriptor; its hash is the target key
    pub payload: Oid,          // store tree hash; opaque here
    pub payload_kind: String,  // names the payload's store schema kind; attest never fetches it
    pub key: Oid,              // claim id of the key-add/rotate claim for the signing key
}
```

The claim commit's tree is `Envelope` serialized through the general codec (envelopes are documents, not identities — they may evolve; only `Target` is frozen).
Signature bytes are on the commit (`gpgsig` header), not in the tree: putting them in the tree would make the signed content contain its own signature.
Signing a claim is `Store::with_signer(...)` + `commit_forward` on the claim ref — attest adds zero signing code.

`payload_kind` is a label for consumers (query joins it against `refs/schema/<kind>`); attest carries it without understanding it, exactly like the payload hash.

### 2. Target: an opaque `{kind, id}` descriptor, frozen through the normal form

```rust
#[derive(Facet)]
#[facet(facet_git_tree::identity_key)]
pub struct Target {
    pub kind: String,   // "blob" | "commit" | "tree" | "anchor" | ... — a label, never interpreted
    pub id: Oid,        // the object or identity hash the label qualifies
}
```

The target key is `normal_form::hash(Target)`, and the identity-universe check at schema registration keeps `Target` inside the frozen universe.
Attest never interprets `kind`: "targets are typed per check" means the *consumer* (a query rule, a forge check) decides what a `"blob"` target means.
`anchor:7f3e` in the CLI parses to `Target { kind: "anchor", id: 7f3e }`; anchor vocabulary stays out of this crate — a kind string is not a dependency.

Phase-1 targets are single-hash only.
Commit ranges and `(base, tip)` pairs are deliberately excluded: the doc flags range/pair target-key derivation as the open hard question, one problem shared with target syntax.
`Target` as a two-field struct is forward-compatible — a phase-2 range target is a new `kind` whose `id` is the normal-form hash of a range descriptor, and no phase-1 key changes.

### 3. Ref layout: `refs/claims/<target-key>`, claim id = commit oid

One ref per target key, hex-named.
Claims on a target chain as commits on that ref; `log <target>` is a parent walk; the claim id is the commit's own oid, so no side mapping exists (same rule the interim query store already follows).
`sign` advances the ref via CAS; a lost race retries on the new tip, so concurrent claims on one target serialize instead of forking.

git-query's interim `refs/meta/claims/<hex>` (one ref per claim, no chain) is superseded; the migration is a query-side change of `CLAIMS_PREFIX` plus re-backing, not an attest concern.
`refs/claims/*` matches the doc's own write-set examples (`refs/claims/action/kiln/*` — executors get per-vocabulary subtrees under the same namespace; the target-key layout applies within each subtree).

### 4. Native vocabulary: revocation and key lifecycle, and nothing else

Revocation and key add/rotate are the only claim kinds attest understands, because they are the envelope machinery itself:

- **revocation**: a claim whose target is `{kind: "claim", id: <claim id>}`, appended to the *revoked claim's own ref* so the chain records it.
  `resolve <target>` returns the chain with structurally revoked claims marked — marked, not deleted, and with no opinion on what revocation *means* downstream.
- **key add / rotate**: claims whose payloads are key docs — an attest-owned store schema (`AttestKey { format: String, public_key: Bytes, ... }`), the one payload schema attest registers, because key material is envelope machinery.
  `Envelope.key` points at the key-add claim; rotation chains on the key's own claim ref.

Everything else — rebind pins (anchor's schema, already registered by `gix-anchor/src/pin.rs`), action records, review assertions — is an opaque payload.
If a function in `gix-attest` ever matches on a payload kind other than its own two, that is the abstraction leaking.

### 5. `verify` is crypto only; validity never enters this repo

`verify(claim)` re-serializes the commit without its `gpgsig` header, resolves `Envelope.key` through the key chain (crypto-verifying each link), and checks the signature bytes against the key.
That is the entire function.

`key_valid_at` — was the key valid at the op-log position where the claim was admitted — is a query predicate over op-log order, and retroactivity of key revocation is rule policy.
Neither belongs here, and `gix-attest` exposes nothing that would let a caller confuse "cryptographically sound" with "valid."
The result type says so: `Verified` / `BadSignature` / `UnknownKeyFormat`, no `Valid` variant to misread.

### 6. Verification is pluggable by key format, with one format shipped

`Signer` produces opaque bytes; something must interpret them, and that something is attest's one piece of crypto:

```rust
pub trait Verifier {
    fn verify(&self, signed: &[u8], signature: &SignatureBytes, key: &AttestKey) -> Verdict;
}
```

Dispatch is on `AttestKey.format`, cross-checked against the armor preamble of the stored block (`SSH SIGNATURE` vs `PGP SIGNATURE`) — the same dispatch git itself performs on `gpgsig` contents.
Ship `ssh-ed25519` (armored SSHSIG blocks via the `ssh-key` crate) — git's own signing ecosystem; the stored block is byte-for-byte what `ssh-keygen -Y sign` produces, so `ssh-keygen -Y verify` *and* `git verify-commit` are both non-Rust test oracles, as `../git-store/docs/specification.adoc` requires.
One format, one implementation; the trait exists because key *formats* genuinely vary, not to anticipate a second engine.

### 7. What attest must never contain

The nothing-duplicated ledger.
Each row names the owner; re-implementing the right column in this workspace is a defect, not a convenience.

| concern | owner | attest's relationship |
|---|---|---|
| the hash function, normal form | store (`facet-git-tree`) | calls it |
| signing mechanics, signature transport | store (`Signer` seam, `gpgsig` header) | writes through it, reads it back |
| ref writes, CAS, layout | store (`commit_forward`, `Layout`) | calls it |
| payload schemas (rebind pin, action, review) | their vocabulary owners | carries hashes, never fetches |
| claim validity, revocation *semantics*, thresholds | query rules | none |
| enforcement (which namespaces require signatures) | boundary hooks / gate server, policy as query | none |
| anchor vocabulary (`AnchorId`, `Binding`) | `gix-anchor` | none — sibling isolation, CI-enforced |
| trailer parsing | nobody — dies with query's interim store | replaces it |

## Crate layout

```text
crates/gix-attest       envelope.rs   Envelope, Target, target_key()
                        chain.rs      sign (via store), log walk, resolve, revoke
                        key.rs        AttestKey schema, key-chain resolution, registration
                        verify.rs     Verifier trait, ssh-ed25519 impl, Verdict
                        schema.rs     register attest's own kinds at refs/schema/*
crates/git-attest       main.rs       sign | revoke | verify | log | resolve
                                      target syntax: <kind>:<hex>
```

`gix-attest` deps: `gix-store`, `gix-refstore`, `facet-git-tree`, `facet`, `gix`, `ssh-key`, `thiserror`.
Not `gix-anchor`, not anything from `../git-query`.
Conventions verbatim from this workspace: edition 2024, resolver 3, `#![forbid(unsafe_code)]`, `MIT OR Apache-2.0`, path deps intra-repo, git deps to `git-store`.

## Consumer contract

What query's host layer (and later forge) gets, so `claim.rs` can be re-backed without reaching into internals:

```rust
Claims::open(&store)                          // over a Layout whose claims prefix is refs/claims
  .log(&target) -> impl Iterator<Item = Claim>          // chain walk, newest first
  .resolve(&target) -> Vec<Claim>                        // revocations applied structurally
  .verify(&claim) -> Verdict                             // crypto only
Claim { id: Oid, envelope: Envelope, revoked_by: Option<Oid> }
```

Query re-derives its EDB from this: `claim/1`, `kind/2` (from `payload_kind`), `target/2`, `signer/2` (from `key`) — and reads pin payloads through store's codec using anchor's registered schema, composing the two siblings for the first time.

## Phases

### Phase 0 — store-side, one PR in `../git-store`: `gpgsig` transport

Move `write_commit`'s signature storage from the hex `signature` extra header to the standard `gpgsig` header (armored block, git's continuation-line encoding); `Store::signature` reads it back.
Acceptance: `git log --show-signature` / `git verify-commit` succeed on a store-written ssh-signed commit with `gpg.ssh.allowedSignersFile` configured.

### Phase 1 — solo: envelope, target key, chain

`Envelope`, `Target`, `target_key`, schema registration with the universe check exercised in tests, `sign` through a caller-supplied `Signer`, `log`, ref layout.
No crypto yet — `verify` unimplemented, not stubbed-true.
Differential fixture: a claim written by `gix-attest` is readable by `git cat-file` and `git interpret-trailers`-free — plain commits, plain trees.

### Phase 2 — two agents, parallel

- **verify**: `AttestKey`, key chain, `Verifier`, ssh-ed25519, `Verdict`; test vectors cross-checked against `ssh-keygen -Y verify` as the oracle, the same shell-out-to-the-real-tool discipline as `test-support` everywhere else.
- **native kinds**: revocation append + `resolve`, key rotate chaining.

### Phase 3 — solo: CLI

`git attest` with the five subcommands over the library; interactive schema-driven input reuses store's `DocumentBuilder` — the dynamic write path exists once.

### Phase 4 — coordinated: query migration

Lands in `../git-query` (its plan: `DEVPLAN-effect.md` Part A), gated on Phases 1–2 here.
Attest-side work is limited to API adjustments the migration surfaces.

## Definition of done

- A claim signed here verifies here and via `ssh-keygen -Y verify`; a bit-flipped commit fails.
- `resolve` on a target with a revoked claim marks exactly that claim.
- `cargo metadata` check proving no `gix-anchor` ↔ `gix-attest` edge is in CI.
- `../git-query`'s `claim.rs` seam comment can be deleted — its replacement compiles against the consumer contract above.
- DELTA X1 rewritten from ABSENT to a delta list or removed.

## Open questions

- Range/pair targets (= target syntax phase 2): one problem, still open, still deferred; `Target`'s shape is chosen so the answer is additive.
- Machine keys: the doc says the signer distinguishes machine from human actors — is that a field on `AttestKey`, a key-naming convention, or a claim kind?
  Recommendation: a field on `AttestKey`, since enforcement rules will want to query it.
- Claim-ref deletion under the server's ref-deletion policy (open in `ARCHITECTURE.md`): if claim refs can be deleted, `Envelope.key` can dangle; forbidding deletion under `refs/claims/*` specifically is the cheap answer even if heads stay deletable.
- When attest moves to its own repo: after forge consumes it, before publishing — same trigger shape as the effect split.
