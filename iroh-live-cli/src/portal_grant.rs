//! Keeps the desktop portal's screen-sharing grants across runs.
//!
//! The portal returns a restore token with each grant. A later run that passes
//! the token back can reuse the grant without showing the picker again, on a
//! compositor that keeps grants. The tokens live in one file per source kind
//! under the CLI's config directory. The file follows the token of the last
//! selection opened: it is rewritten when the portal grants a new token, and
//! deleted when moq-video holds none, because an open used the token and
//! failed, or the stream ended (the user stopped sharing, the stream failed,
//! or the source went away).

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use iroh_live::media::video::capture::portal::{Kind, RestoreToken, Selection};
use n0_future::task::AbortOnDropHandle;
use tracing::{debug, warn};

/// The watcher of each source kind's last selection, by [`name`].
static WATCHERS: LazyLock<Mutex<HashMap<&'static str, AbortOnDropHandle<()>>>> =
    LazyLock::new(Default::default);

/// Returns a selection of `kind`, seeded with the token an earlier run stored.
pub(crate) fn selection(kind: Kind) -> Selection {
    let selection = Selection::new(kind);
    let Some(path) = path(kind) else {
        return selection;
    };
    match std::fs::read_to_string(&path) {
        Ok(token) if !token.trim().is_empty() => {
            debug!(path = %path.display(), "seeding the portal with a stored grant");
            selection.with_restore_token(RestoreToken::new(token.trim()))
        }
        Ok(_) => selection,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => selection,
        Err(err) => {
            warn!(path = %path.display(), %err, "cannot read the stored portal grant");
            selection
        }
    }
}

/// Stores `selection`'s token now, and again whenever it changes.
///
/// The watcher does not keep the selection alive. It stores the last token
/// and stops once every clone is gone, which happens when the open failed or
/// the stream has ended. It is also replaced by the watcher of the next
/// selection of the same kind, so the file follows the selection opened last.
pub(crate) fn watch(kind: Kind, selection: &Selection) {
    let Some(path) = path(kind) else {
        return;
    };
    let mut tokens = selection.restore_token();
    let task = tokio::spawn(async move {
        store(&path, tokens.token().as_ref());
        while let Some(token) = tokens.changed().await {
            store(&path, token.as_ref());
        }
    });
    WATCHERS
        .lock()
        .expect("poisoned")
        .insert(name(kind), AbortOnDropHandle::new(task));
}

/// Writes `token` to `path`, readable only by this user, or deletes the file
/// for no token.
fn store(path: &Path, token: Option<&RestoreToken>) {
    let result = match token {
        Some(token) => write_private(path, token.as_str()),
        None => match std::fs::remove_file(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        },
    };
    match result {
        Ok(()) => debug!(path = %path.display(), stored = token.is_some(), "portal grant synced"),
        Err(err) => warn!(path = %path.display(), %err, "cannot store the portal grant"),
    }
}

/// Writes `contents` to `path` with mode 0600, creating its directory.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_bytes())
}

/// Returns the file that holds the grant for `kind`, if the platform has a
/// config directory.
fn path(kind: Kind) -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("iroh-live")
            .join("portal")
            .join(format!("{}.token", name(kind))),
    )
}

/// Returns the file name stem for `kind`.
fn name(kind: Kind) -> &'static str {
    match kind {
        Kind::Screen => "screen",
        Kind::Window => "window",
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn store_writes_a_private_file_and_deletes_it_for_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portal").join("screen.token");
        store(&path, Some(&RestoreToken::new("grant")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "grant");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        store(&path, None);
        assert!(!path.exists());
        // Deleting a file that is already gone is not an error.
        store(&path, None);
    }
}
