# git-attest

`git attest` — sign, revoke, and read claims: signed envelopes chained on claim refs, over [`gix-attest`](../gix-attest).

```text
git attest sign    <target> <payload-tree> --kind <payload-kind>
git attest revoke  <claim-id>
git attest verify  <claim-id>          # crypto only
git attest log     <target>
git attest resolve <target>
```

Targets are written `<kind>:<hex>` — `anchor:7f3e` is `Target { kind: "anchor", id: 7f3e }`.
The kind is a label, never interpreted and never checked against a list: anchor vocabulary is not a dependency of attest, and CI proves the dependency graph agrees.

`sign` signs by shelling out to `ssh-keygen -Y sign -n git` with `--signing-key`, or with git's own `user.signingkey` under `gpg.format = ssh`.
The armored SSHSIG block lands in the claim commit's standard `gpgsig` header, so `git log --show-signature` and `git verify-commit` read claims with none of this installed.
The signing key is published as a key claim the first time it signs; `--key <claim-id>` names a specific link after a rotation.

A payload may be an existing tree hash, or a document built for `--kind` with `--json` or `--interactive` — written through `gix-store`'s dynamic write path.

`verify` reports cryptography and nothing else: `Verified` (exit 0), `BadSignature` (exit 1), `UnknownKeyFormat` (exit 2).
A sound signature is not a valid claim — revocation is `resolve`'s answer, and admissibility is a query rule's.
