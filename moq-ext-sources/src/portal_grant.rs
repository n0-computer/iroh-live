//! The desktop portal's screen-sharing grant, kept in a file across runs.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use moq_video::capture::portal::{Kind, RestoreToken, Selection};
use tracing::{debug, warn};

/// A desktop portal grant kept in a file, so a later run can skip the picker.
///
/// [`PortalGrant::selection`] seeds a [`Selection`] with the stored restore
/// token and stores each new token the portal grants. On a compositor that
/// keeps grants, the next run then shares the same screen or window without
/// asking.
///
/// The file survives an open that is killed or refused, since the open takes
/// the token before asking the portal. Once the portal has granted a new
/// token, the end of the stream (for example the user stopped sharing)
/// deletes the file, so the next run asks again, unless
/// [`keep_after_end`](Self::keep_after_end) is set.
///
/// The file holds the token as text, readable only by its owner.
/// [`VideoOpenOptions::with_portal_grants`](crate::spec::VideoOpenOptions::with_portal_grants)
/// keeps grants for sources opened from a specifier.
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
    keep_after_end: bool,
}

impl PortalGrant {
    /// Creates a grant of `kind` stored in the file at `path`.
    pub fn new(kind: Kind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
            keep_after_end: false,
        }
    }

    /// Keeps the stored grant when the stream ends, until a new one replaces it.
    ///
    /// The next run then tries the stored token, and the portal asks only if
    /// the compositor no longer honors it.
    #[must_use]
    pub fn keep_after_end(mut self) -> Self {
        self.keep_after_end = true;
        self
    }

    /// Creates a grant of `kind` stored in the user's config directory.
    ///
    /// The file is `<app>/portal/{screen,window}.token` under
    /// `$XDG_CONFIG_HOME`, or `~/.config` when that is unset or not absolute.
    /// Returns `None` without either, or when `app` is not a single file name.
    pub fn in_config_dir(kind: Kind, app: &str) -> Option<Self> {
        if app.is_empty() || Path::new(app).file_name() != Some(app.as_ref()) {
            warn!(app, "the portal grant's app name is not a file name");
            return None;
        }
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
    /// A missing or unreadable file gives a selection without a token, so the
    /// portal asks. New tokens are stored by a task that ends with the last
    /// clone of the selection; outside a tokio runtime they are not stored.
    pub fn selection(&self) -> Selection {
        let mut selection = Selection::new(self.kind);
        if let Some(token) = self.load() {
            debug!(path = %self.path.display(), "seeding the portal with a stored grant");
            selection = selection.with_restore_token(token);
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                path = %self.path.display(),
                "no tokio runtime, so new portal grants are not stored",
            );
            return selection;
        };
        let path = self.path.clone();
        let keep_after_end = self.keep_after_end;
        let mut tokens = selection.restore_token();
        runtime.spawn(async move {
            let mut granted = false;
            while let Some(token) = tokens.changed().await {
                apply(&path, update(&mut granted, keep_after_end, token.as_ref()));
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

/// What to do with the stored file when the selection's token changes.
#[derive(Debug, PartialEq, Eq)]
enum Update<'a> {
    /// Writes the new token.
    Store(&'a RestoreToken),
    /// Leaves the file as it is.
    Keep,
    /// Deletes the file.
    Delete,
}

/// Decides what a change of the token to `token` does to the file.
///
/// `granted` is set by the first token. A `None` before it is an open taking
/// the stored token; a `None` after it is the end of the stream.
fn update<'a>(
    granted: &mut bool,
    keep_after_end: bool,
    token: Option<&'a RestoreToken>,
) -> Update<'a> {
    match token {
        Some(token) => {
            *granted = true;
            Update::Store(token)
        }
        None if *granted && !keep_after_end => Update::Delete,
        None => Update::Keep,
    }
}

/// Carries out `update` on the file at `path`.
fn apply(path: &Path, update: Update<'_>) {
    match update {
        Update::Store(token) => match write_private(path, token.as_str()) {
            Ok(()) => debug!(path = %path.display(), "portal grant stored"),
            Err(err) => warn!(path = %path.display(), %err, "cannot store the portal grant"),
        },
        Update::Keep => {
            debug!(path = %path.display(), "the portal grant has no new token, keeping the stored one");
        }
        Update::Delete => match std::fs::remove_file(path) {
            Ok(()) => debug!(path = %path.display(), "portal grant ended, deleted"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => warn!(path = %path.display(), %err, "cannot delete the portal grant"),
        },
    }
}

/// Writes `contents` to `path` with mode 0600 through a renamed temporary file.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())?;
    drop(file);
    std::fs::rename(&temp, path)
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
    fn write_private_replaces_the_file_with_a_private_one() {
        let dir = test_dir("write");
        let path = dir.join("portal").join("screen.token");
        write_private(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn none_keeps_the_file_until_the_first_grant_and_deletes_it_after() {
        let token = RestoreToken::new("grant");
        let mut granted = false;
        // The open takes the stored token, or the compositor refuses it.
        assert_eq!(update(&mut granted, false, None), Update::Keep);
        assert_eq!(update(&mut granted, false, None), Update::Keep);
        // The portal grants a token, and later the user stops sharing.
        assert_eq!(
            update(&mut granted, false, Some(&token)),
            Update::Store(&token)
        );
        assert_eq!(update(&mut granted, false, None), Update::Delete);
        // A reopen takes and replaces the token again.
        assert_eq!(
            update(&mut granted, false, Some(&token)),
            Update::Store(&token)
        );
    }

    #[test]
    fn keep_after_end_keeps_the_file_on_every_none() {
        let token = RestoreToken::new("grant");
        let mut granted = false;
        assert_eq!(update(&mut granted, true, None), Update::Keep);
        assert_eq!(
            update(&mut granted, true, Some(&token)),
            Update::Store(&token)
        );
        // The stream ends: the file stays until a new grant replaces it.
        assert_eq!(update(&mut granted, true, None), Update::Keep);
        assert_eq!(update(&mut granted, true, None), Update::Keep);
        let next = RestoreToken::new("next");
        assert_eq!(
            update(&mut granted, true, Some(&next)),
            Update::Store(&next)
        );
    }

    #[test]
    fn keep_after_end_is_off_by_default() {
        let grant = PortalGrant::new(Kind::Screen, "/tmp/screen.token");
        assert!(!grant.keep_after_end);
        assert!(grant.keep_after_end().keep_after_end);
    }

    #[test]
    fn apply_stores_keeps_and_deletes_the_file() {
        let dir = test_dir("apply");
        let path = dir.join("window.token");
        let token = RestoreToken::new("grant");
        apply(&path, Update::Store(&token));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "grant");
        apply(&path, Update::Keep);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "grant");
        apply(&path, Update::Delete);
        assert!(!path.exists());
        apply(&path, Update::Delete);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn selection_outside_a_runtime_is_seeded_without_a_task() {
        let dir = test_dir("no-runtime");
        let grant = PortalGrant::new(Kind::Screen, dir.join("screen.token"));
        write_private(grant.path(), "grant").unwrap();
        let token = grant.selection().restore_token().token();
        assert_eq!(token, Some(RestoreToken::new("grant")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn selection_is_seeded_with_the_stored_token() {
        let dir = test_dir("seed");
        let grant = PortalGrant::new(Kind::Window, dir.join("window.token"));
        assert_eq!(grant.selection().restore_token().token(), None);

        write_private(grant.path(), "grant\n").unwrap();
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

    #[test]
    fn in_config_dir_takes_only_a_file_name() {
        for app in ["", ".", "..", "../x", "a/b", "/tmp/x"] {
            assert_eq!(
                PortalGrant::in_config_dir(Kind::Window, app),
                None,
                "{app:?} is not a file name",
            );
        }
    }
}
