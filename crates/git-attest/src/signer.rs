//! The CLI's signing path: `ssh-keygen -Y sign -n git`, and nothing else.
//!
//! `gix-attest` contains no signing code — signing is `gix-store`'s
//! [`Signer`](gix_store::Signer) seam, and the *choice* of signer is a user
//! interface concern, so it lives here. What this module produces is the
//! armored SSHSIG block `ssh-keygen -Y sign` writes, verbatim: exactly what the
//! `gpgsig` transport stores, what `gix_attest::verify::SshEd25519` checks, and
//! what stock `git verify-commit` accepts.
//!
//! No ssh-agent is involved: the private key file is read by `ssh-keygen`
//! itself, so a passphrase-protected key prompts on the terminal the way every
//! other `ssh-keygen` invocation does.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use gix_store::{SignatureBytes, Signer};

/// The signing key a run of `git attest` uses: the private key `ssh-keygen`
/// signs with, and the OpenSSH public line the key claim publishes.
#[derive(Debug, Clone)]
pub struct SigningKey {
    /// The private key file passed to `ssh-keygen -Y sign -f`.
    private: PathBuf,
    /// The public key, as an `ssh-ed25519 AAAA… comment` line.
    public: String,
}

impl SigningKey {
    /// The key for this run: `--signing-key` when given, else git's own
    /// configuration — `user.signingkey`, with `gpg.format` required to be
    /// `ssh`, because an armored block of another format is not something this
    /// CLI can produce.
    ///
    /// A `--signing-key` value may name either half of the pair: `id_ed25519`
    /// or `id_ed25519.pub` select the same key, since the private half is what
    /// signs and the public half is what is published.
    pub fn resolve(repo: &gix::Repository, flag: Option<&Path>) -> Result<Self> {
        let path = match flag {
            Some(path) => path.to_owned(),
            None => Self::from_config(repo)?,
        };
        Self::at(&path)
    }

    /// The key `user.signingkey` names, having checked `gpg.format`.
    fn from_config(repo: &gix::Repository) -> Result<PathBuf> {
        let config = repo.config_snapshot();
        let format = config
            .string("gpg.format")
            .map(|value| value.to_string())
            .unwrap_or_else(|| "openpgp".to_owned());
        if format != "ssh" {
            bail!(
                "gpg.format is {format:?}: this CLI signs with ssh keys only \
                 (set `gpg.format = ssh`, or pass --signing-key)"
            );
        }
        let key = config
            .string("user.signingkey")
            .ok_or_else(|| anyhow!("no user.signingkey configured; pass --signing-key"))?;
        Ok(PathBuf::from(key.to_string()))
    }

    /// The key at `path`, whichever half of the pair it names.
    fn at(path: &Path) -> Result<Self> {
        let private = match path.extension().is_some_and(|ext| ext == "pub") {
            true => path.with_extension(""),
            false => path.to_owned(),
        };
        if !private.exists() {
            bail!("no private key at {}", private.display());
        }
        let public = public_line(&private)?;
        Ok(SigningKey { private, public })
    }

    /// The OpenSSH public key line, for `AttestKey::from_openssh`.
    pub fn public(&self) -> &str {
        &self.public
    }

    /// A [`Signer`] over this key, to hand to
    /// [`Store::with_signer`](gix_store::Store::with_signer).
    pub fn signer(&self) -> SshKeygen {
        SshKeygen {
            key: self.private.clone(),
        }
    }
}

/// `<key>.pub` when it is on disk, else the line `ssh-keygen -y` derives from
/// the private key — the same two places git looks.
fn public_line(private: &Path) -> Result<String> {
    let sidecar = PathBuf::from(format!("{}.pub", private.display()));
    if let Ok(line) = std::fs::read_to_string(&sidecar) {
        return Ok(line.trim().to_owned());
    }
    let out = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(private)
        .output()
        .context("running ssh-keygen -y")?;
    if !out.status.success() {
        bail!(
            "ssh-keygen -y failed for {}: {}",
            private.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Signs commit bytes by shelling out to `ssh-keygen -Y sign -n git`.
///
/// The block returned is `ssh-keygen`'s own output, byte for byte: the
/// `gpgsig` transport stores it unchanged, so the bytes git verifies are the
/// bytes `ssh-keygen` wrote.
pub struct SshKeygen {
    key: PathBuf,
}

impl Signer for SshKeygen {
    type Error = std::io::Error;

    fn sign(&self, bytes: &[u8]) -> Result<SignatureBytes, Self::Error> {
        // `-n git` is git's own SSHSIG namespace — the one
        // `gix_attest::verify` and `git verify-commit` both require — and the
        // payload goes over stdin so the signed bytes never touch disk.
        let mut child = Command::new("ssh-keygen")
            .args(["-Y", "sign", "-q", "-n", "git", "-f"])
            .arg(&self.key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("ssh-keygen stdin was not piped"))?
            .write_all(bytes)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "ssh-keygen -Y sign failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(SignatureBytes::from(out.stdout))
    }
}
