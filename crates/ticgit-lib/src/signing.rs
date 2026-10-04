//! SSH-based operation signing, git-signing style, via `ssh-keygen -Y`.
//!
//! Step 5 of the op-log roadmap (ticket `2ea37f`). Signing reuses the user's
//! existing SSH key (ed25519) exactly as git's SSH commit signing does, so
//! agent / passphrase handling is inherited and no private key material ever
//! touches git-meta — only the public key (in the identity chain) and the
//! armored signature (in the op envelope) are stored.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// Signature namespace, mirroring git's `gpg.ssh` use of a fixed namespace.
const NAMESPACE: &str = "ticgit";

/// Sign `data` with the SSH private key at `key_path`, returning the armored
/// SSH signature (`-----BEGIN SSH SIGNATURE----- …`).
pub fn sign(data: &[u8], key_path: &Path) -> Result<String> {
    let dir = tempfile::tempdir().map_err(|e| Error::Signing(e.to_string()))?;
    let datafile = dir.path().join("op");
    std::fs::write(&datafile, data).map_err(|e| Error::Signing(e.to_string()))?;

    let status = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("sign")
        .arg("-n")
        .arg(NAMESPACE)
        .arg("-f")
        .arg(key_path)
        .arg(&datafile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Signing(format!("spawning ssh-keygen: {e}")))?;
    if !status.success() {
        return Err(Error::Signing("ssh-keygen -Y sign failed".to_string()));
    }

    std::fs::read_to_string(dir.path().join("op.sig"))
        .map_err(|e| Error::Signing(format!("reading signature: {e}")))
}

/// Derive the public key line (`ssh-ed25519 AAAA…`) from a private key via
/// `ssh-keygen -y`. Published to the identity chain so other clones can verify.
pub fn public_key(key_path: &Path) -> Result<String> {
    let out = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(key_path)
        .output()
        .map_err(|e| Error::Signing(format!("spawning ssh-keygen -y: {e}")))?;
    if !out.status.success() {
        return Err(Error::Signing("ssh-keygen -y failed".to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Verify `signature` over `data` was produced by `identity` using
/// `public_key` (an authorized-keys-style line, e.g. `ssh-ed25519 AAAA…`).
/// Returns `Ok(true)` for a good signature, `Ok(false)` for a bad one, and
/// `Err` only when ssh-keygen itself could not be run.
pub fn verify(data: &[u8], signature: &str, identity: &str, public_key: &str) -> Result<bool> {
    let dir = tempfile::tempdir().map_err(|e| Error::Signing(e.to_string()))?;
    let sigfile = dir.path().join("op.sig");
    std::fs::write(&sigfile, signature).map_err(|e| Error::Signing(e.to_string()))?;
    let allowed = dir.path().join("allowed_signers");
    std::fs::write(&allowed, format!("{identity} {}\n", public_key.trim()))
        .map_err(|e| Error::Signing(e.to_string()))?;

    let mut child = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("verify")
        .arg("-n")
        .arg(NAMESPACE)
        .arg("-I")
        .arg(identity)
        .arg("-f")
        .arg(&allowed)
        .arg("-s")
        .arg(&sigfile)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Signing(format!("spawning ssh-keygen: {e}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| Error::Signing("ssh-keygen stdin unavailable".to_string()))?
        .write_all(data)
        .map_err(|e| Error::Signing(format!("writing data to ssh-keygen: {e}")))?;
    let status = child
        .wait()
        .map_err(|e| Error::Signing(format!("waiting for ssh-keygen: {e}")))?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn gen_key(dir: &Path) -> (PathBuf, String) {
        let key = dir.join("id_ed25519");
        let status = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "tester@ticgit", "-q", "-f"])
            .arg(&key)
            .status()
            .expect("spawn ssh-keygen keygen");
        assert!(status.success(), "key generation failed");
        let pubkey = std::fs::read_to_string(dir.join("id_ed25519.pub"))
            .expect("read pubkey")
            .trim()
            .to_string();
        (key, pubkey)
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pubkey) = gen_key(dir.path());
        let sig = sign(b"operation bytes", &key).unwrap();
        assert!(verify(b"operation bytes", &sig, "tester@ticgit", &pubkey).unwrap());
    }

    #[test]
    fn verify_rejects_tampered_data() {
        let dir = tempfile::tempdir().unwrap();
        let (key, pubkey) = gen_key(dir.path());
        let sig = sign(b"operation bytes", &key).unwrap();
        assert!(!verify(b"different bytes", &sig, "tester@ticgit", &pubkey).unwrap());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let (key, _pubkey) = gen_key(dir.path());
        let sig = sign(b"operation bytes", &key).unwrap();

        let other = tempfile::tempdir().unwrap();
        let (_k2, pubkey2) = gen_key(other.path());
        assert!(!verify(b"operation bytes", &sig, "tester@ticgit", &pubkey2).unwrap());
    }
}
