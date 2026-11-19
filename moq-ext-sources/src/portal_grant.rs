//! The desktop portal's screen-sharing grant, kept in a file across runs.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use moq_video::capture::portal::{Kind, RestoreToken, Selection};
use tracing::{debug, warn};

/// A desktop portal grant kept in a file, so a later run can skip the picker.
///
/// The portal returns a restore token with each grant. A run that passes the
/// token back reuses the grant without showing the picker, on a compositor
/// that keeps grants. [`PortalGrant::selection`] seeds a [`Selection`] with
/// the stored token and stores each new token the portal grants.
///
/// The file is kept when moq-video holds no token. moq-video drops its token
/// while an open uses it, so deleting the file then would lose the grant to a
/// run killed during the open, or to one the compositor refuses (GNOME does
/// while the screen is locked), and the next unattended run would wait on the
/// picker. A stale token costs nothing, since the portal ignores it and asks
/// again.
///
/// The file holds the token as text, readable only by its owner.
///
/// # Examples
///
/// ```no_run
/// # async fn example() -> Result<(), moq_ext_sources::Error> {
/// use moq_ext_sources::{PortalGrant, VideoSource};
/// use moq_video::capture::{self, portal::Kind};
///
/// let grant = PortalGrant::in_config_dir(Kind::Screen, "my-app").expect("a home directory");
/// let mut config = capture::Config::default();
/// config.source = capture::Source::Portal(grant.selection());
/// let screen = VideoSource::capture(config).await?;
/// # let _ = screen;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalGrant {
    kind: Kind,
    path: PathBuf,
}

impl PortalGrant {
    /// Creates a grant of `kind` stored in the file at `path`.
    pub fn new(kind: Kind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }

    /// Creates a grant of `kind` stored in the user's config directory.
    ///
    /// The file is `<app>/portal/screen.token` or `<app>/portal/window.token`
    /// under `$XDG_CONFIG_HOME`, or under `~/.config` when that is unset or
    /// not absolute. Returns `None` without either.
    pub fn in_config_dir(kind: Kind, app: &str) -> Option<Self> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|home| home.is_absolute())
                    .map(|home| home.join(".config"))
            })?;
        let name = match kind {
            Kind::Screen => "screen.token",
            Kind::Window => "window.token",
        };
        Some(Self::new(kind, config.join(app).join("portal").join(name)))
    }

    /// Returns the kind of source the grant is for.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Returns the file that holds the grant.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a selection seeded with the stored token, which stores each new one.
    ///
    /// A task watches the selection's token and writes each new token to the
    /// file. It does not keep the selection alive, and ends once every clone
    /// of the selection is dropped. A missing or unreadable file gives a
    /// selection without a token, so the portal asks.
    ///
    /// # Panics
    ///
    /// Panics outside a tokio runtime.
    pub fn selection(&self) -> Selection {
        let mut selection = Selection::new(self.kind);
        if let Some(token) = self.load() {
            debug!(path = %self.path.display(), "seeding the portal with a stored grant");
            selection = selection.with_restore_token(token);
        }
        let path = self.path.clone();
        let mut tokens = selection.restore_token();
        tokio::spawn(async move {
            while let Some(token) = tokens.changed().await {
                store(&path, token.as_ref());
            }
        });
        selection
    }

    /// Reads the stored token, or `None` if there is none.
    fn load(&self) -> Option<RestoreToken> {
        match std::fs::read_to_string(&self.path) {
            Ok(token) if !token.trim().is_empty() => Some(RestoreToken::new(token.trim())),
            Ok(_) => None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                warn!(path = %self.path.display(), %err, "cannot read the stored portal grant");
                None
            }
        }
    }
}

/// Writes `token` to `path`, readable only by this user. Without a token, keeps the file.
fn store(path: &Path, token: Option<&RestoreToken>) {
    let Some(token) = token else {
        debug!(path = %path.display(), "no portal grant held now, keeping the stored one");
        return;
    };
    match write_private(path, token.as_str()) {
        Ok(()) => debug!(path = %path.display(), "portal grant stored"),
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Returns a fresh directory for one test, removed by the caller.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("portal-grant-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn store_writes_a_private_file_and_keeps_it_without_a_token() {
        let dir = test_dir("store");
        let path = dir.join("portal").join("screen.token");
        store(&path, Some(&RestoreToken::new("grant")));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "grant");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        store(&path, None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "grant");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn selection_is_seeded_with_the_stored_token() {
        let dir = test_dir("seed");
        let grant = PortalGrant::new(Kind::Window, dir.join("window.token"));
        assert_eq!(grant.selection().restore_token().token(), None);

        store(grant.path(), Some(&RestoreToken::new("grant\n")));
        let token = grant.selection().restore_token().token();
        assert_eq!(token, Some(RestoreToken::new("grant")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn in_config_dir_names_the_file_after_the_app_and_kind() {
        let Some(grant) = PortalGrant::in_config_dir(Kind::Screen, "app") else {
            return;
        };
        assert!(grant.path().ends_with("app/portal/screen.token"));
        assert!(grant.path().is_absolute());
        assert_eq!(grant.kind(), Kind::Screen);
    }
}
