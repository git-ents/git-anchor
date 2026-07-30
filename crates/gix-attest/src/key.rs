//! Key lifecycle: [`AttestKey`], its schema registration, and key-chain
//! resolution.
//!
//! [`AttestKey`] is the one payload schema attest registers, because key
//! material *is* envelope machinery: [`Envelope::key`](crate::Envelope::key)
//! names the claim that published the key a claim was signed with, so a
//! verifier that could not read a key doc could not verify anything.
//!
//! A key is a chain, not an identity. The key-add claim is the chain's root;
//! every rotation is another claim on the *same* ref — the key's own claim ref,
//! `refs/claims/<target-key of {kind: "key", id: <key id>}>` — whose
//! [`Envelope::key`](crate::Envelope::key) names the link it rotates away
//! from. Resolving a link therefore walks from the root up to it and
//! crypto-verifies every rotation against its predecessor's key, so a rotation
//! signed by anything other than the key it replaces resolves to a verdict
//! instead of a key.
//!
//! What does not belong here: whether a key was *valid* at the moment a claim
//! was admitted. That is `key_valid_at`, a query predicate over op-log order,
//! and the retroactivity of key revocation is rule policy. Neither enters
//! this crate. The chain's root is likewise not judged: whether to trust a
//! key-add at all is policy, and cryptography has nothing to say about it.

use facet::Facet;
use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_store::{Committer, RefStore};

use crate::chain::{Claims, KEY_TARGET_KIND};
use crate::error::{Error, Result};
use crate::schema::{KEY_KIND, key_segment};
use crate::verify::{Verdict, Verifier, verify_commit};

/// A signing key, as attest publishes it: a format label and the public key
/// bytes that label describes.
///
/// The format is what [`Verifier`](crate::verify::Verifier) implementations
/// dispatch on, and `public_key` is opaque to everything but the
/// implementation that claims the format — for the shipped
/// [`SshEd25519`](crate::verify::SshEd25519) that is an SSH public key in its
/// raw binary encoding, the same bytes base64 in an `ssh-ed25519 AAAA…` line.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct AttestKey {
    /// The key format, e.g. [`SSH_ED25519`](crate::verify::SSH_ED25519).
    /// Verification dispatches on it.
    pub format: String,
    /// The public key, in whatever encoding [`format`](Self::format) names.
    pub public_key: Vec<u8>,
    /// Whether the key belongs to a machine actor rather than a human one.
    ///
    /// Recorded, never interpreted: enforcement rules that care about the
    /// distinction are query's, and this crate only carries the bit so they
    /// have somewhere to read it from.
    pub machine: bool,
}

impl AttestKey {
    /// An [`AttestKey`] from an OpenSSH public key line — `ssh-ed25519 AAAA…
    /// user@host`, the contents of an `id_ed25519.pub`.
    ///
    /// The stored [`public_key`](Self::public_key) is that line's key data in
    /// its raw binary encoding, and [`format`](Self::format) is the key's own
    /// algorithm name — so a key `ssh-keygen` generated is published without
    /// anyone transcribing it, and the format label is the one the key states
    /// rather than one a caller asserts.
    ///
    /// # Errors
    ///
    /// [`Error::KeyMaterial`] when `openssh` is not an OpenSSH public key.
    pub fn from_openssh(openssh: &str, machine: bool) -> Result<Self> {
        let public = ssh_key::PublicKey::from_openssh(openssh)
            .map_err(|source| Error::KeyMaterial(source.to_string()))?;
        Ok(AttestKey {
            format: public.algorithm().as_str().to_owned(),
            public_key: public
                .to_bytes()
                .map_err(|source| Error::KeyMaterial(source.to_string()))?,
            machine,
        })
    }
}

/// One link of a key chain: the claim that published a key, and the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    /// The claim id of the key-add or rotate claim.
    pub(crate) claim: ObjectId,
    /// The key that claim published.
    pub(crate) key: AttestKey,
}

/// The outcome of resolving a key chain: a key, or the verdict that stopped
/// the walk.
///
/// The verdict is a crypto failure on a *rotation* — a link not signed by the
/// key it replaces — and it is returned rather than swallowed so
/// [`Claims::verify`] reports it in place of a verdict on the claim itself: a
/// claim signed by an unreachable key is not verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// The key the requested link published, with every rotation below it
    /// crypto-verified.
    Key(AttestKey),
    /// A rotation below the requested link did not verify.
    Rejected(Verdict),
}

impl<R, O> Claims<'_, R, O>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    /// Publish `key` as a key-add claim, the root of its own key chain, and
    /// return the claim id — the value
    /// [`Envelope::key`](crate::Envelope::key) carries.
    ///
    /// The key's identity is the content hash of its document, so adding the
    /// identical key twice chains onto the one key ref rather than starting a
    /// second one. The key document is also written as an ordinary store
    /// entity under `refs/keys/<key id>`, which is what keeps it reachable —
    /// an envelope carries its payload's hash, not the payload.
    ///
    /// The claim's own [`Envelope::key`](crate::Envelope::key) is the null oid:
    /// a chain's root has no predecessor to name, and whether to trust it is
    /// policy rather than cryptography.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] when the key document or the claim cannot be written —
    /// including [`gix_store::Error::NoSchema`] when
    /// [`register_key_schema`](crate::register_key_schema) has not run —
    /// and [`Error::RefName`]/[`Error::NormalForm`] for the derived names.
    pub fn add_key(&self, key: &AttestKey) -> Result<ObjectId> {
        let payload = self.write_key(key)?;
        let target = crate::Target {
            kind: KEY_TARGET_KIND.to_owned(),
            id: payload.into(),
        };
        let envelope = crate::Envelope {
            target: target.clone(),
            payload: payload.into(),
            payload_kind: KEY_KIND.to_owned(),
            key: ObjectId::null(gix::hash::Kind::Sha1).into(),
        };
        self.append(&target, &envelope)
    }

    /// Rotate the key `previous` published: publish `key` as the next link of
    /// the same key chain, and return the new claim id.
    ///
    /// The claim lands on `previous`'s own claim ref — the key's chain — and
    /// names `previous` as the key it was signed with. The store must
    /// therefore be signing with `previous`'s key for the rotation to resolve:
    /// that check is [`resolve_key`](Self::resolve_key)'s, performed on every
    /// read, and this write does not anticipate it.
    ///
    /// # Errors
    ///
    /// [`Error::NotAKeyClaim`] when `previous` is not a key claim, plus
    /// [`add_key`](Self::add_key)'s errors.
    pub fn rotate_key(&self, previous: ObjectId, key: &AttestKey) -> Result<ObjectId> {
        let envelope = self.key_envelope(previous)?;
        let payload = self.write_key(key)?;
        let rotation = crate::Envelope {
            target: envelope.target.clone(),
            payload: payload.into(),
            payload_kind: KEY_KIND.to_owned(),
            key: previous.into(),
        };
        self.append(&envelope.target, &rotation)
    }

    /// The key `claim` published, with every rotation below it in its chain
    /// crypto-verified.
    ///
    /// This is the resolution [`verify`](Self::verify) performs on
    /// [`Envelope::key`](crate::Envelope::key), exposed because a caller that
    /// wants to display a claim's signing key should read the same key
    /// verification read.
    ///
    /// # Errors
    ///
    /// [`Error::NotAKeyClaim`] when `claim` does not publish a key,
    /// [`Error::ClaimOffChain`] when it is not on the chain of the key it
    /// claims to belong to, and [`Error::Store`] when a read fails.
    pub fn key(&self, claim: ObjectId) -> Result<Option<AttestKey>> {
        Ok(match self.resolve_key(claim)? {
            Resolved::Key(key) => Some(key),
            Resolved::Rejected(_) => None,
        })
    }

    /// [`key`](Self::key), keeping the verdict that stopped the walk.
    pub(crate) fn resolve_key(&self, claim: ObjectId) -> Result<Resolved> {
        self.resolve_key_with(claim, None)
    }

    /// [`resolve_key`](Self::resolve_key), checking rotations with `verifier`
    /// when one is supplied instead of the shipped dispatch table.
    pub(crate) fn resolve_key_with(
        &self,
        claim: ObjectId,
        verifier: Option<&dyn Verifier>,
    ) -> Result<Resolved> {
        let links = self.key_chain(claim)?;
        // The root is the trust anchor: nothing below it can be checked, and
        // whether to trust it is policy. Every rotation above it is checked
        // against the key it rotates away from.
        for pair in links.windows(2) {
            let [previous, rotation] = pair else {
                unreachable!("windows(2) yields pairs")
            };
            match verify_commit(self, rotation.claim, &previous.key, verifier)? {
                Verdict::Verified => {}
                verdict => return Ok(Resolved::Rejected(verdict)),
            }
        }
        let last = links
            .into_iter()
            .next_back()
            .expect("a key chain has at least the requested link");
        Ok(Resolved::Key(last.key))
    }

    /// The chain of key links from its root up to and including `claim`,
    /// oldest first.
    fn key_chain(&self, claim: ObjectId) -> Result<Vec<Link>> {
        let envelope = self.key_envelope(claim)?;
        let chain = self.log(&envelope.target)?;
        // Newest first, so the requested link's predecessors are everything
        // after it; reversed, the walk runs root-first.
        let mut links = Vec::new();
        let mut seen = false;
        for entry in chain {
            if entry.id == claim {
                seen = true;
            }
            if seen {
                links.push(Link {
                    claim: entry.id,
                    key: self.key_document(entry.id, &entry.envelope)?,
                });
            }
        }
        if !seen {
            return Err(Error::ClaimOffChain { claim });
        }
        links.reverse();
        Ok(links)
    }

    /// The envelope of `claim`, refusing anything that is not a key claim.
    fn key_envelope(&self, claim: ObjectId) -> Result<crate::Envelope> {
        let envelope = self.envelope_at(claim)?;
        self.key_document(claim, &envelope)?;
        Ok(envelope)
    }

    /// The key document a key claim's envelope points at.
    fn key_document(&self, claim: ObjectId, envelope: &crate::Envelope) -> Result<AttestKey> {
        if envelope.payload_kind != KEY_KIND || envelope.target.kind != KEY_TARGET_KIND {
            return Err(Error::NotAKeyClaim {
                claim,
                payload_kind: envelope.payload_kind.clone(),
            });
        }
        Ok(self
            .store()
            .kind::<AttestKey>(key_segment())
            .decode(envelope.payload.into())?)
    }

    /// Write `key` as a store entity and return its document tree — the key's
    /// content id, and the payload hash a key claim carries.
    fn write_key(&self, key: &AttestKey) -> Result<ObjectId> {
        let kind = self.store().kind::<AttestKey>(key_segment());
        // `compile` yields the very tree a write commits, so the entity name
        // is the document's own hash and the write is idempotent.
        let tree = kind.compile(key)?;
        let name = gix_store::RefSegment::new(tree.to_string())?.into();
        kind.put(&name, key)?;
        Ok(tree)
    }
}
