# The git-* product family

Typed documents as Git trees.
Derivation and authority are kept strictly separate.

Six products.
Library crates are `gix-*`, binaries are `git-*`.

| product | adds | depends on |
|---|---|---|
| `git-store` | typed schema + value trees, signing seam | git |
| `git-anchor` | content identity (Binding) + bind oracles | store |
| `git-attest` | signed claim envelopes + chains | store |
| `git-query` | Datalog over refs, docs, bindings, claims | store, anchor, attest |
| `git-effect` | rules whose heads are not derivable | store, attest, query |
| `git-forge` | application layer (comments, reviews, issues, ...) | all of the above |

`anchor` and `attest` are siblings.
Neither knows the other exists.
Attest targets and payloads are opaque; anchor vocabulary in claims (pins) is a store schema registered by anchor, composed only in query.

## Core invariant

Authority is a property of ref namespace membership.
A namespace is declared authoritative or derived; signed ref transitions are how authoritative namespaces advance.
Signatures are the enforcement mechanism, not the definition — derived state may carry machine signatures without becoming authoritative.

Test for any piece of state: *delete it — can it be rederived with zero information loss?*

- Yes → derived.
  Cacheable, unsigned or machine-signed, GC-able.
  Never an attestation target, never an effect trigger.
- No → authoritative.
  Exists only because someone signed it into existence.

Mechanism and enforcement are split:

- mechanism: store's write path carries opaque signature bytes (`Signer` trait).
  Store never interprets them.
- verification: attest, crypto only.
- enforcement: boundary (hooks / server receive).
  Which ref namespaces require valid signatures is expressed as rules — policy is a query.

Signature requirements key on ref namespace, not document kind.
Machine writes use machine keys; the signer distinguishes machine from human actors.

### Server model (interim)

- One gate server per repo is the linearization point.
  It accepts signed pushes, is fast-forward-only, and applies ref transitions by CAS-on-expected-tip with retry.
  Multi-server linearization is deferred.
- The server writes an operation-log ref recording every admitted transition.
  The op-log is **authoritative**: server-signed, and the ordering authority for validity decisions.
  The server key is the root of trust; the resulting auditability gap is the one earmarked for gittuf (or equivalent) later.
- ff-only covers advances, not deletion.
  If a ref with unmerged history is deleted, anchors whose `genesis_rev` becomes unreachable lose identity verification and dedup; inline hints keep resolution alive.
  Whether the server forbids ref deletion is open (below).

## Identity rule

Identity contains only non-derivable coordinates.
Anything computable from them is a hint, and hints are never identity-bearing.
Corollary: algorithm or grammar version bumps must never change an identity.

Capture-time canonicalization is legal: producing coordinates carefully (e.g. snapping a selection to a CST node's byte span at capture) is not derivation.
Once captured, coordinates are frozen; later grammar bumps change nothing already captured — they only mean new captures of "the same node" may land on different bytes.

### Identity normal form

Identity- and key-bearing subtrees (anchor identity, action key) are restricted to a closed primitive universe: scalars, byte strings, hashes, lists, maps.
No enums, no dynamic facet values, nothing schema-rich.
Only the mapping from this universe to git bytes is frozen — entry naming and scalar encodings; git's sorted tree entries provide ordering.
The general codec evolves freely because identity subtrees never exercise it.

Constraint this imposes: action `params` must be expressible in the normal form.
Enforced at schema registration.

## Abstractions → products

| abstraction | home |
|---|---|
| typed tree, meta-ref | store |
| Binding (identity + hints) | anchor |
| claim envelope | attest |
| claim kinds (payload schemas) | store schemas, registered by vocabulary owners |
| rule | query |
| effect | effect |
| action (record) | store schema; provenance via attest — not a product |
| signed push, signature bytes | git substrate + store seam; attest verifies |

---

## git-store

Schemas and values as typed Git trees.
`facet-git-tree` is the pure codec; dynamic facet values for JSON-like objects.
Identity of any document is the hash of its compiled typed tree.

Also owns, as library:

- the signing seam: opaque signature bytes on writes, `Signer` trait,
  never interpreted here
- the dynamic write path: schema fetch, struct-field walk, interactive prompting, `serialize_value_with_schema`.
  Other CLIs (anchor included) consume this; it exists once
- the one hash function.
  Action cache keys and anchor ids are store tree hashes of designated subtrees — no separate key crates.
  Identity- and key-bearing subtrees are hashed through the identity normal form (above); general documents through the full codec
- the identity normal form: the frozen mini-codec and its type-universe
  check

Does not: authority decisions, derivation, ref-advance policy.

```text
git store put   <schema> <value>     # -> tree hash
git store get   <tree-ish>           # -> typed value
git store check <tree-ish> <schema>
```

Open: home of schemas themselves (schema ref vs type-registry ref).

## git-anchor

An anchor is not a stored entity.
It is a `Binding` subtree embedded inline in the referring document (comment, review, ...).
By-reference anchors are rejected: a document holding an anchor id can dangle, and the anchored material stops being reachable from the document's own tree (retention).

```text
Binding {
  identity: {            # anchor id = store hash of this subtree
                         #   (via identity normal form)
    genesis_rev,         # commit where captured
    path,
    span,                # byte range over the blob bytes as stored
                         #   (post-clean-filter) — always canonical
  },
  hints: {               # sibling subtree; additive, versioned, never identity-bearing
    fingerprints: [ { algo, params, value }, ... ],
    descriptors:  [ { grammar id+version, node kind, name path }, ... ],
  },
}
```

Identity is the three non-derivable coordinates, nothing else.
Fingerprints are pure functions of identity given the repo (they fail the rederive test); descriptors are functions of blob + grammar.
Both are hints.
Consequences:

- grammar or fingerprint-algorithm upgrades never change anchor ids, so
  they never orphan pins
- identity is content-addressed: byte-identical captures at the same genesis coincide.
  This is a collision, not a promise — independent captures of "the same thing" that differ by a byte are distinct anchors with distinct pin histories, and comments track independently (the Gerrit/GitHub status quo).
  Deterministic capture tooling (snapping) raises the collision rate; nothing depends on it.
  Anchors have no creator — authorship belongs to the referring document
- convergence for display is derived, not identity-level: the forge groups
  comments by bound location at the viewed rev
- hints ride inline, so re-anchoring works in shallow/partial clones
  without genesis reachability; missing hints degrade to on-demand
  recomputation

Future additive option, query-layer only, if manual-pin fatigue appears: a derived `equiv(A, B)` predicate over identity overlap + hints, with pin borrowing in bind at `conf = pin_conf × equiv_conf`.
No identity change; safe to defer.

`git anchor create` is a pure emitter: computes identity + capture-time hints, writes objects at most, advances nothing.
The distinguishing CLI capability is binding capture and injection; generic document writing is store's dynamic write path.

Oracles (`gix-anchor`), pure functions of `(objects, Binding, params)`:

| oracle | mechanism | notes |
|---|---|---|
| op-log | external operation log (seam; DeltaDB adapter later) | highest fidelity |
| diff-trace | map span through hunks along first-parent walk | diff algorithm + params pinned as oracle params |
| fingerprint | fuzzy content search in target tree | uses hint fingerprints, recomputes if absent; returns candidates |

`project` = the pin-free oracle chain.
Library-internal.
It returns candidates with `(oracle, confidence)` and applies no thresholds — anchor holds no policy, mirroring attest.
No user-facing command resolves through `project`; resolution is query's `bind/5`.

## git-attest

Claims as chained commits on claim refs.
Attest owns the envelope: target id, payload tree hash, signer, chain.
Targets and payloads are opaque.
Verification is cryptographic only — no policy.

Native vocabulary (envelope machinery itself):

| kind | meaning |
|---|---|
| revocation | invalidates a prior claim (chained) |
| key add / rotate | key lifecycle |

Everything else is a payload: a store schema registered by whoever owns the vocabulary — `rebind pin` by anchor, `action record` by the action schema, review assertions by forge.
Attest carries them without understanding them.

Validity vs verification: `verify` is crypto-only.
Claim *validity* (`key_valid_at`) is a query predicate over op-log admission order — a claim is valid iff its signing key was valid at the op-log position where the claim was admitted.
Whether revoking a key retroactively invalidates already-admitted claims signed by it is rule policy, to be decided explicitly (open questions).

Targets are typed per check: blob, tree/subtree, commit, commit range, hybrid (parent commit + body tree).
Target syntax is the open hard question, and it is the same problem as attest phase-2 range/pair target-key derivation — one problem, not two.

```text
git attest sign    <target> <payload-tree>
git attest revoke  <claim-id>
git attest verify  <claim-id>          # crypto only
git attest log     <target>
git attest resolve <target>
```

## git-query

Moded Datalog.
One `git-query` binary.

Base facts (EDB):

- git objects and refs: commits, trees, ref states, op-log records
- typed docs (store)
- bindings (anchor)
- claims (attest)
- extension-contributed predicates (e.g. `cst_node/4` from a tree-sitter
  extension, `reviewed/1` from forge rules)

Rule modules: one per ref under `refs/meta/rules/*`, with `pub` visibility markers.
Engine: Nemo behind an engine-agnostic seam, magic-set demand loop for moded builtins, naive evaluator retained as oracle.
Validation: 9 passes plus effect-stratification over declared write sets (below).

`bind/5` is the only user-facing resolution.
Pin and oracles feed one confidence lattice; pin is an oracle at confidence 1.0.
No negation: candidate generation is monotone, so semi-naive incrementality holds for it; max-selection sits in its own aggregation stratum, recomputed per touched anchor.

```text
cand(A, Rev, Loc, pin, 1.0) :- pin_claim(A, Rev, Loc).
cand(A, Rev, Loc, O, C)     :- project(A, Rev, Loc, O, C).   # oracle chain from gix-anchor

bind(A, Rev, Loc, O, C)     :- cand(A, Rev, Loc, O, C),
                               C = max{ C' : cand(A, Rev, _, _, C') }.   # aggregation stratum
```

A pin dominates at 1.0 by construction.
Orphaning is a rule decision: the confidence threshold is a rule parameter here, not anchor behavior.
Below threshold → surfaced for manual pin.
Orphaned beats silently wrong.
Forge reads binds through query only.

Results go to one of three places:

1. ephemeral — stdout/API.
   The default.
2. cache ref — memoized fixpoint under `refs/query/cache/*`.
   Keyed on `(rule module tree hash, input ref states, engine version, params)`.
   Infrastructure: unsigned/machine-signed, GC-able, never an attestation target, never an effect trigger.
   Incremental: semi-naive deltas over append-only refs for monotone strata; aggregation strata recomputed per touched key.
3. promotion — a claim or effect that cites the query + inputs.
   The only door from derived to authoritative.

```text
git query run     <rule> [args]
git query explain <rule> [args]
git query rule    <subcommands>
```

## git-effect

An effect is a rule whose head is not derivable.

- query rule: `body ⊢ head` — head is a derived fact, free.
- effect: `body ⇒ obligation` — head is an authoritative ref transition,
  which must be performed by an authorized executor and signed.

Charter, exhaustively: watch query predicates, check authorization (claims + gate rules, itself a query), invoke executors, record signed transitions.
No scheduler ambitions, no executor implementations.

Every effect declares its full write set: the ref namespaces it — and any executor it invokes — may advance, including claim refs for emitted action records.
The boundary rejects executor writes outside the declaration.

Mechanics:

- Trigger detection = semi-naive delta evaluation on ref advance.
  No separate watcher.
  Triggers are predicates over authoritative refs only.
  Push intents are therefore reified as signed refs (`refs/intent/*`) so gates can trigger on them; the effect system is the ref transaction manager at the server.
- Idempotence key: executor-invoking effects key on the action key of the
  invoked run; pure advances key on the store hash of
  `(effect id, trigger delta)` — same normal-form hash, no key crate.
- Effect chaining = the feedback loop: signed transition → new facts →
  new deltas → further effects.
- Fork-bomb freedom: stratification over declared read/write namespace sets — decidable by construction, checked as a validation pass in git-query, because effects share the rule body language.
  Executor side channels are covered because emitted claims are inside the declared write set.

Placement: lives in query's repo until the rule language settles — every effect change touches that language.
Split out after.

```text
git effect define <doc>       # typed effect doc, stored via store
git effect status
git effect log                # signed transition records
```

## action (not a product)

A hermetic derivation record — a store typed doc:

```text
Action {
  key: {                 # action key = store hash of this subtree
                         #   (via identity normal form)
    executor,            # id + version
    inputs,              # pinned content hashes
    params,              # everything else that affects output — engine
                         # version, diff algorithm, grammar version.
                         # Nothing ambient. Must be expressible in the
                         # identity normal form.
  },
  output,                # content hash
}
```

The key is a store tree hash of a designated subtree — same move as anchor identity, no dedicated key crate.
Query cache keys and effect idempotence keys are this one hash.

Instances: query evaluation, build steps, env materialization.
Same record schema, different executors, *different execution strategies* — the shape is unified, the runtime is not.

Op-log records are likewise store typed docs: codec in store, base-fact predicates in query, anchor's op-log oracle consumes them via store.
One format, one parser.
The op-log ref itself is server-written and authoritative (server model, above).

Action vs effect, type-level:

| | action | effect |
|---|---|---|
| rederivable | yes | no |
| signed | optional (machine) | required |
| cacheable | yes | idempotence-keyed, not cached |
| relation | cited by effects/claims | may invoke executors that emit actions |

## git-forge

Application layer.
Composes everything, owns no primitive logic.

- comment = forge doc embedding a Binding subtree; anchor id falls out as the hash of its identity subtree.
  Authorship, edit history, and body are the document's; the Binding is shared identity only on byte-identical capture.
  Display convergence is by bound location
- review targets: blob, commit, tree/subtree, commit range, (base, tip) pairs
- `reviewed/1` at blob granularity = derived predicate, mode-independent
- merge gates = effects gated on `reviewed` + policy claims
- issues, agent branches, web UI

Current layout note: `gix-comment`, `git-comment`, `gix-comment-lsp`, and the editor integration are forge-layer code living in the anchor repo as its first consumer.
Acknowledged; move at the code-freeze rewrite.

---

## Worked examples

### 1. Review comment that travels

```text
# reviewer comments on a function at rev A
git anchor create A src/refdb.rs 4180..4630
# -> Binding: identity {A, src/refdb.rs, 4180..4630} + capture-time hints
#    anchor id 7f3e = hash(identity subtree). Nothing advanced.

git store put comment.schema '{binding: <inline 7f3e>, body: "..."}'
# forge advances refs/forge/comment/42; the write carries signature bytes
# via store's Signer seam

# branch is rebased; rev B has disconnected history
git query run bind 7f3e B
# -> (Loc: src/refdb.rs 4412..4862, Oracle: fingerprint, Conf: 0.93)

# file later split; fingerprint returns two candidates, conf 0.44
# -> below the rule's threshold: orphaned, surfaced for manual pin
git attest sign anchor:7f3e <rebind-payload: rev C -> src/refdb/store.rs 118..568>
# envelope: attest. payload: store schema registered by anchor.

git query run bind 7f3e C
# -> (Oracle: pin, Conf: 1.0)      # pin dominates the lattice
```

A second reviewer capturing byte-identical coordinates at rev A gets anchor id 7f3e — a content-address collision, not a guarantee.
A selection differing by one byte is a distinct anchor with its own pin history; both comments still display at the same bound location.

### 2. Derived review state gating a merge

```text
# rule module in refs/meta/rules/review
pub reviewed(Blob) :-
    approval_claim(Claim, Target), covers(Target, Blob),
    !revoked(Claim), key_valid_at(Claim).        # validity per op-log order

# effect doc
effect merge_gate {
  when:   push_intent(Ref, Tip),     # reified signed ref under refs/intent/*
  gate:   forall Blob in changed_blobs(Ref, Tip): reviewed(Blob),
  then:   advance(Ref, Tip),         # signed transition by authorized executor
  writes: [Ref],
}
```

`reviewed` is free and rederivable.
The ref advance is not — it exists only because the gate executor signed it.

### 3. Effect chain with actions (CI-shaped)

```text
effect build_on_push {
  when:   advanced(refs/heads/main, Tip),
  gate:   authorized(build_executor),
  then:   run(kiln, inputs: tree_of(Tip)),
  writes: [refs/claims/action/kiln/*],     # executor-emitted action records
}
# executor emits Action{key:{kiln, tree, params}, out} -> attested as action record

effect release_on_green {
  when:   action_record(kiln, tree_of(Tip), _, Out), tests_pass(Out),
  gate:   authorized(release_executor),
  then:   advance(refs/release/candidate, Tip),
  writes: [refs/release/candidate],
}
```

Chaining: effect 1's attested action record is a fact that triggers effect 2 — and it is inside effect 1's declared write set, so the stratification check sees it.
Stratification over declared read/write namespace sets proves this graph terminates: `refs/release/candidate` is written but appears in no trigger's read set.

Note `when` of effect 2 reads the *action record claim* (authoritative, signed), not the build cache (derived).

---

## Dependency and data flow

```text
                git-forge
                    |
                git-effect ----invokes----> executors --emit--> action records
                    |                                                |
                git-query <----- reads claims, bindings, docs -------+
                /       \
        git-anchor    git-attest
                \       /
                git-store  (typed trees, Signer seam, the hash)
                    |
                   git
```

Reads flow up through query.
Authority flows only through signed transitions on authoritative namespaces: signature bytes produced via store's seam, verified by attest, required at boundaries per rules, linearized by the gate server (interim: CAS-and-retry, ff-only, server-written op-log).

## Open questions

- schema home: dedicated schema ref vs type-registry ref
- attest target syntax == phase-2 range/pair target-key derivation (one
  problem)
- entity id for mutable forge docs (genesis-empty-blob + follow-on commit,
  or not) — distinct from anchor identity, which is settled
- retroactivity: does key revocation invalidate already-admitted claims signed by that key?
  Rule policy — decide before forge review semantics land
- server ref-deletion policy: forbid (nothing GC'd, genesis always
  reachable) vs allow (anchors on deleted unmerged branches lose identity
  verification; hints keep resolution alive)
- op-log recording for pushes outside the gate server: the store seam gives
  boundary hooks a mechanism; who ships and owns the hook tooling is
  unassigned
- multi-server linearization (interim CAS-and-retry accepted)
- timing of the git-effect split (after rule language settles) and the
  comment-crate move (code-freeze rewrite)
