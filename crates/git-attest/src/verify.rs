//! Verification: the [`Verifier`] trait, its `ssh-ed25519` implementation, and
//! [`Verdict`].
//!
//! Verifying a claim is: re-serialize the claim commit without its `gpgsig`
//! header, resolve [`Envelope::key`](crate::Envelope::key) through
//! [`crate::key`]'s chain, and check the stored signature block against the
//! key — dispatched on the key's format, cross-checked against the armor
//! preamble, exactly as git dispatches on `gpgsig` contents. That is the
//! entire function, and [`Claims::verify`](crate::Claims::verify) is where it
//! is spelled.
//!
//! The result type is named so it cannot be misread: [`Verdict::Verified`] /
//! [`Verdict::BadSignature`] / [`Verdict::UnknownKeyFormat`], and no `Valid`
//! variant. Claim *validity* — was the signing key valid at the op-log
//! position where the claim was admitted, and does revoking a key reach back
//! over claims already admitted — is a query predicate and a policy question
//! respectively. Neither is answerable here, and nothing here answers it
//! partially.

use gix::ObjectId;
use gix::objs::{Find, Write};
use gix_store::{Committer, RefStore, SignatureBytes, Store};

use crate::chain::Claims;
use crate::error::{Error, Result};
use crate::key::{AttestKey, Resolved};

/// What cryptography has to say about a claim, and nothing more.
///
/// There is deliberately no `Valid`: a caller cannot mistake a sound signature
/// for an admissible claim, because this type has no word for the latter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The signature is the signing key's, over these commit bytes.
    Verified,
    /// The signature is not the key's over these bytes — a forgery, a
    /// tampered commit, a signature that does not parse, or no signature at
    /// all. Cryptography distinguishes none of those from each other.
    BadSignature,
    /// No shipped [`Verifier`] claims the key's format, or the stored block's
    /// armor disagrees with it. Nothing was checked, and nothing is asserted.
    UnknownKeyFormat,
}

/// A check of one signature format.
///
/// The trait exists because key *formats* genuinely vary — git itself
/// dispatches on the contents of `gpgsig` — not to anticipate a second engine
/// behind one format. One format ships: [`SshEd25519`].
pub trait Verifier {
    /// Check `signature` against `key` over `signed`, the commit bytes as they
    /// stood before the `gpgsig` header was added.
    fn verify(&self, signed: &[u8], signature: &SignatureBytes, key: &AttestKey) -> Verdict;
}

/// The [`AttestKey::format`] the shipped [`SshEd25519`] verifier claims.
pub const SSH_ED25519: &str = "ssh-ed25519";

/// The SSHSIG namespace git signs and verifies commits under, and therefore
/// the only one a claim commit's block may declare.
const GIT_NAMESPACE: &str = "git";

/// The armor preamble an SSHSIG block opens with, cross-checked against the
/// key's format the way git cross-checks a `gpgsig` block against
/// `gpg.format`.
const SSHSIG_PREAMBLE: &str = "-----BEGIN SSH SIGNATURE-----";

/// `ssh-ed25519` over armored SSHSIG blocks: git's own signing ecosystem, so
/// the bytes checked here are byte-for-byte what `ssh-keygen -Y sign` produces
/// and what `ssh-keygen -Y verify` and `git verify-commit` accept.
#[derive(Debug, Clone, Copy, Default)]
pub struct SshEd25519;

impl Verifier for SshEd25519 {
    fn verify(&self, signed: &[u8], signature: &SignatureBytes, key: &AttestKey) -> Verdict {
        if key.format != SSH_ED25519 {
            return Verdict::UnknownKeyFormat;
        }
        let Ok(armor) = std::str::from_utf8(signature.as_bytes()) else {
            return Verdict::UnknownKeyFormat;
        };
        // The armor must agree with the key's format before any bytes are
        // checked: a `PGP SIGNATURE` block under an ssh key is a format
        // mismatch, not a bad signature.
        if !armor.trim_start().starts_with(SSHSIG_PREAMBLE) {
            return Verdict::UnknownKeyFormat;
        }
        let Ok(public) = ssh_key::PublicKey::from_bytes(&key.public_key) else {
            return Verdict::UnknownKeyFormat;
        };
        let Ok(block) = armor.parse::<ssh_key::SshSig>() else {
            return Verdict::BadSignature;
        };
        // The namespace is part of what SSHSIG signs, and git's is `git`: a
        // block signed for another namespace is not a signature over this
        // commit as a commit.
        if block.namespace() != GIT_NAMESPACE {
            return Verdict::BadSignature;
        }
        match public.verify(GIT_NAMESPACE, signed, &block) {
            Ok(()) => Verdict::Verified,
            Err(_) => Verdict::BadSignature,
        }
    }
}

/// The shipped verifier for `format`, or `None` when no shipped one claims it.
///
/// The dispatch table, all one row of it: dispatch is on
/// [`AttestKey::format`], and a format nobody claims is
/// [`Verdict::UnknownKeyFormat`] rather than a guess.
#[must_use]
pub fn verifier_for(format: &str) -> Option<&'static dyn Verifier> {
    match format {
        SSH_ED25519 => Some(&SshEd25519),
        _ => None,
    }
}

/// The bytes a commit's signature covers: the commit object, minus the one
/// `gpgsig` header.
///
/// Git's rule and the store's rule coincide — the header cannot be inside the
/// bytes it attests to — so this is the exact inverse of the store's write
/// path, performed on the raw object so the round trip is byte-preserving
/// rather than a re-encoding that happens to agree. Header continuation lines
/// (git's leading space) belong to the header they continue, and the body
/// after the blank line is untouched.
fn signed_bytes(commit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(commit.len());
    let mut rest = commit;
    let mut dropping = false;
    loop {
        let (line, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(end) => (&rest[..=end], &rest[end + 1..]),
            None => (rest, &[][..]),
        };
        if line == b"\n" || line.is_empty() {
            // The header section is over; the message is verbatim.
            out.extend_from_slice(line);
            out.extend_from_slice(next);
            return out;
        }
        if line.starts_with(b" ") {
            if !dropping {
                out.extend_from_slice(line);
            }
        } else {
            dropping = line.starts_with(b"gpgsig ") || line == b"gpgsig\n";
            if !dropping {
                out.extend_from_slice(line);
            }
        }
        rest = next;
    }
}

/// Check `commit`'s signature against `key`, with `verifier` or — when it is
/// `None` — whichever shipped one claims the key's format.
///
/// Shared by [`Claims::verify`](crate::Claims::verify) and key-chain
/// resolution, which is the same check applied to a rotation.
pub(crate) fn verify_commit<R, O>(
    claims: &Claims<'_, R, O>,
    commit: ObjectId,
    key: &AttestKey,
    verifier: Option<&dyn Verifier>,
) -> Result<Verdict>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let Some(verifier) = verifier.or_else(|| verifier_for(&key.format)) else {
        return Ok(Verdict::UnknownKeyFormat);
    };
    let Some(signature) = claims.store().signature(commit)? else {
        // An unsigned commit is not a soundly signed one, and there is no
        // third answer to give: `Verdict` has no room for "nothing to check"
        // on purpose.
        return Ok(Verdict::BadSignature);
    };
    let signed = signed_bytes(&raw_commit(claims.store(), commit)?);
    Ok(verifier.verify(&signed, &signature, key))
}

/// The raw bytes of the commit object `id`, without the loose-object header.
fn raw_commit<R, O>(store: &Store<R, O>, id: ObjectId) -> Result<Vec<u8>>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let mut buf = Vec::new();
    let data = store
        .objects()
        .try_find(&id, &mut buf)
        .map_err(|source| Error::Object { claim: id, source })?
        .ok_or(Error::MissingClaim { claim: id })?;
    if data.kind != gix::objs::Kind::Commit {
        return Err(Error::NotAClaim { claim: id });
    }
    Ok(data.data.to_vec())
}

impl<R, O> Claims<'_, R, O>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    /// The cryptographic verdict on `claim`, and nothing else.
    ///
    /// The claim commit is re-serialized without its `gpgsig` header, the
    /// signing key is resolved from [`Envelope::key`](crate::Envelope::key)
    /// through its own chain — crypto-verifying every rotation on the way —
    /// and the stored signature block is checked against that key. A crypto
    /// failure anywhere on the key chain is reported in place of a verdict on
    /// the claim: a claim signed by an unreachable key is not verified.
    ///
    /// This says nothing about whether the claim is *valid*. Validity is
    /// `key_valid_at`, a query predicate over op-log admission order, and it
    /// is not expressible in [`Verdict`].
    ///
    /// # Errors
    ///
    /// [`Error::NotAKeyClaim`] when the envelope's key does not name a key
    /// claim, [`Error::ClaimOffChain`] when that claim is not on its key's
    /// chain, and [`Error::Store`]/[`Error::Object`]/[`Error::MissingClaim`]
    /// when a claim or key document cannot be read.
    pub fn verify(&self, claim: &crate::Claim) -> Result<Verdict> {
        self.verify_dispatched(claim, None)
    }

    /// [`verify`](Self::verify) with a caller-supplied [`Verifier`], for a key
    /// format this crate does not ship.
    ///
    /// The trait's whole purpose: the shipped dispatch table has one row, and
    /// a caller holding a second format's implementation needs no fork to use
    /// it. `verifier` is asked about the claim and about every rotation of its
    /// key chain alike.
    ///
    /// # Errors
    ///
    /// As [`verify`](Self::verify).
    pub fn verify_with(&self, claim: &crate::Claim, verifier: &dyn Verifier) -> Result<Verdict> {
        self.verify_dispatched(claim, Some(verifier))
    }

    /// The one implementation of both: resolve the key, then check the claim.
    fn verify_dispatched(
        &self,
        claim: &crate::Claim,
        verifier: Option<&dyn Verifier>,
    ) -> Result<Verdict> {
        match self.resolve_key_with(claim.envelope.key.into(), verifier)? {
            Resolved::Rejected(verdict) => Ok(verdict),
            Resolved::Key(key) => verify_commit(self, claim.id, &key, verifier),
        }
    }
}

/// The commit bytes `commit`'s signature covers, for callers implementing a
/// [`Verifier`] outside this crate.
///
/// The same inverse of the store's write path [`Claims::verify`] uses, exposed
/// so a second format's implementation does not have to re-derive git's rule.
///
/// # Errors
///
/// [`Error::Object`]/[`Error::MissingClaim`]/[`Error::NotAClaim`] when the
/// commit cannot be read.
pub fn signed_payload<R, O>(store: &Store<R, O>, commit: ObjectId) -> Result<Vec<u8>>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    Ok(signed_bytes(&raw_commit(store, commit)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "unit test")]

    use super::*;

    fn key(format: &str, public_key: Vec<u8>) -> AttestKey {
        AttestKey {
            format: format.to_owned(),
            public_key,
            machine: false,
        }
    }

    /// The signed bytes are the commit minus one header, continuation lines
    /// and all — and a commit without the header is its own signed form.
    #[test]
    fn the_signed_bytes_are_the_commit_without_its_gpgsig_header() {
        let plain = b"tree 0000\nauthor A\ncommitter C\n\nmessage\n";
        let signed = b"tree 0000\nauthor A\ncommitter C\ngpgsig -----BEGIN SSH SIGNATURE-----\n \
             AAAA\n -----END SSH SIGNATURE-----\n\nmessage\n";
        assert_eq!(signed_bytes(signed), plain.to_vec());
        assert_eq!(signed_bytes(plain), plain.to_vec());
    }

    /// A `gpgsig` in the *message* is message text, not a header: the header
    /// section ends at the blank line.
    #[test]
    fn a_gpgsig_line_in_the_message_is_left_alone() {
        let commit = b"tree 0000\n\ngpgsig not a header\n";
        assert_eq!(signed_bytes(commit), commit.to_vec());
    }

    #[test]
    fn dispatch_is_on_the_key_format() {
        assert!(verifier_for(SSH_ED25519).is_some());
        assert!(verifier_for("ssh-rsa").is_none());
        assert!(verifier_for("").is_none());
    }

    /// The format label and the armor preamble must agree, and neither
    /// disagreement is reported as a bad signature: nothing was checked.
    #[test]
    fn a_format_nothing_ships_or_disagreeing_armor_checks_nothing() {
        let armored = SignatureBytes::from(
            b"-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n".to_vec(),
        );
        assert_eq!(
            SshEd25519.verify(b"payload", &armored, &key("openpgp", Vec::new())),
            Verdict::UnknownKeyFormat,
            "a key format this verifier does not claim"
        );
        let pgp = SignatureBytes::from(b"-----BEGIN PGP SIGNATURE-----\nAAAA\n".to_vec());
        assert_eq!(
            SshEd25519.verify(b"payload", &pgp, &key(SSH_ED25519, Vec::new())),
            Verdict::UnknownKeyFormat,
            "armor that disagrees with the key's format"
        );
        assert_eq!(
            SshEd25519.verify(b"payload", &armored, &key(SSH_ED25519, vec![0xff; 4])),
            Verdict::UnknownKeyFormat,
            "key bytes that are not an SSH public key"
        );
    }
}
