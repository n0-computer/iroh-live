//! Stored secret keys, and `irl key`.
//!
//! A stored key lives in `<config dir>/iroh-live/secret_keys/<name>.key`,
//! readable by this user only. `irl run` binds with one through
//! `secret_key_name`, `irl publish --key` through its name, and `irl rcan
//! sign --key` signs with one.

use std::path::{Path, PathBuf};

use iroh::SecretKey;
use iroh_live::load_or_create_secret_key;
use n0_error::{Result, anyerr};
use tracing::info;

use crate::args::KeyArgs;

/// Runs `irl key`: prints the public key of a stored key, creating it on first use.
pub(crate) fn run(args: &KeyArgs) -> Result {
    let key = stored_secret_key(&args.name)?;
    println!("{}", key.public());
    Ok(())
}

/// Checks that `name` is a single, non-empty file name.
///
/// `dir.join(name)` escapes `dir` for an absolute name or `..`. This catches
/// typos. It is not a security check.
pub(crate) fn check_key_name(name: &str) -> Result<()> {
    if !name.is_empty() && Path::new(name).file_name() == Some(name.as_ref()) {
        return Ok(());
    }
    Err(anyerr!(
        "the key name {name:?} is not a file name; it names one file in this \
         platform's config directory, so it cannot be a path or empty",
    ))
}

/// Returns the directory the stored keys live in, creating it if needed.
fn key_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyerr!("cannot find this platform's config directory"))?
        .join("iroh-live")
        .join("secret_keys");
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyerr!("failed to create {}: {err}", dir.display()))?;
    Ok(dir)
}

/// Loads the named secret key, generating and storing one on first use.
///
/// A new key's file is readable by this user only.
pub(crate) fn stored_secret_key(name: &str) -> Result<SecretKey> {
    check_key_name(name)?;
    let path = key_dir()?.join(format!("{name}.key"));
    let key = load_or_create_secret_key(&path)
        .map_err(|err| anyerr!("failed to load {}: {err}", path.display()))?;
    info!(name, path = %path.display(), "secret key ready");
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key name that would escape the key directory is refused.
    #[test]
    fn a_key_name_is_one_file_name() {
        check_key_name("laptop").expect("an ordinary name");
        check_key_name("laptop.2").expect("a dot inside a name is still a name");

        for name in ["", "/tmp/evil", "../../evil", "a/b", ".", ".."] {
            assert!(
                check_key_name(name).is_err(),
                "{name:?} is not a file name in the key directory",
            );
        }
    }
}
