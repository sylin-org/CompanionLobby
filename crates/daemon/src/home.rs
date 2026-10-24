//! The data directory.
//!
//! Where the daemon keeps its files:
//!
//! * Windows: the Local AppData known folder, resolved with
//!   `SHGetKnownFolderPath` and never from environment variables, followed by
//!   `CompanionLobby`;
//! * macOS: `~/Library/Application Support/CompanionLobby`;
//! * elsewhere: `$XDG_STATE_HOME/companion-lobby`;
//! * `LOBBY_DATA_DIR` overrides all of them.
//!
//! Any path that is, or lies inside, a `.companion-lobby` directory is refused
//! outright and never read, locked or written. The check runs on the path as
//! the platform resolves it: `..` folded, the trailing dots and spaces
//! Windows ignores stripped, an NTFS stream suffix cut off, and the longest
//! existing prefix canonicalized, so a junction, a symbolic link or a short
//! 8.3 name that leads there is refused too. The daemon, the attach shim,
//! `open` and `stop` all resolve the directory the same way, so a host's shim
//! and the sponsor's own commands meet the same daemon.
//!
//! Whoever creates the directory restricts it to its owner. A directory that
//! is not owner-only is restricted only when everything in it is a file the
//! lobby itself writes; anything else is refused rather than re-permissioned,
//! so a mistyped `LOBBY_DATA_DIR` never rewrites the access of a folder the
//! lobby does not own.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The environment variable that overrides the data directory.
pub const DATA_DIR_ENV: &str = "LOBBY_DATA_DIR";

/// The directory name this product never uses, nor anything inside it.
const REFUSED_COMPONENT: &str = ".companion-lobby";

/// Why the data directory could not be used.
#[derive(Debug)]
pub enum HomeProblem {
    /// The platform reported no base folder.
    NoBaseFolder,
    /// The path is, or lies inside, a directory this product never uses.
    Refused(PathBuf),
    /// The directory could not be created or restricted.
    Io(PathBuf, io::Error),
}

impl fmt::Display for HomeProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBaseFolder => f.write_str(
                "the platform reported no folder for application data; set LOBBY_DATA_DIR",
            ),
            Self::Refused(path) => write!(
                f,
                "{} is or lies inside {REFUSED_COMPONENT}, which Companion Lobby never uses; choose another LOBBY_DATA_DIR",
                display_path(path)
            ),
            Self::Io(path, error) => {
                write!(f, "cannot prepare {}: {error}", display_path(path))
            }
        }
    }
}

impl std::error::Error for HomeProblem {}

/// Resolves the data directory from `LOBBY_DATA_DIR` or the platform folder.
pub fn resolve() -> Result<PathBuf, HomeProblem> {
    let override_dir = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty());
    choose(override_dir.map(PathBuf::from), platform_base())
}

/// The pure decision behind [`resolve`].
pub fn choose(
    override_dir: Option<PathBuf>,
    platform_base: Option<PathBuf>,
) -> Result<PathBuf, HomeProblem> {
    let path = match override_dir {
        Some(path) => path,
        None => platform_base
            .ok_or(HomeProblem::NoBaseFolder)?
            .join(platform_leaf()),
    };
    if refused(&path) {
        return Err(HomeProblem::Refused(path));
    }
    Ok(path)
}

/// Whether `path`, as the platform resolves it, is or lies inside a
/// `.companion-lobby` directory.
pub fn refused(path: &Path) -> bool {
    let resolved = lexical(path);
    names_refused(&resolved) || canonical_prefix(&resolved).is_some_and(|real| names_refused(&real))
}

/// The absolute form of `path` with `.` and `..` folded without touching the
/// file system. On Windows `std::path::absolute` also strips the trailing dots
/// and spaces the Win32 layer ignores.
fn lexical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut folded = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other.as_os_str()),
        }
    }
    folded
}

/// The canonical form of the longest prefix of `path` that exists, which
/// resolves junctions, symbolic links and 8.3 short names.
fn canonical_prefix(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.exists())
        .and_then(|existing| std::fs::canonicalize(existing).ok())
}

fn names_refused(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => is_refused_name(name),
        _ => false,
    })
}

/// Compared without case, ignoring trailing dots and spaces and any `:stream`
/// suffix, the way Windows resolves a name.
fn is_refused_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    let name = name.split(':').next().unwrap_or_default();
    name.trim_end_matches(['.', ' '])
        .eq_ignore_ascii_case(REFUSED_COMPONENT)
}

#[cfg(windows)]
fn platform_base() -> Option<PathBuf> {
    // FOLDERID_LocalAppData through SHGetKnownFolderPath.
    dirs::data_local_dir()
}

#[cfg(target_os = "macos")]
fn platform_base() -> Option<PathBuf> {
    // ~/Library/Application Support
    dirs::data_dir()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_base() -> Option<PathBuf> {
    // $XDG_STATE_HOME, or ~/.local/state when it is unset.
    dirs::state_dir()
}

const fn platform_leaf() -> &'static str {
    if cfg!(any(windows, target_os = "macos")) {
        "CompanionLobby"
    } else {
        "companion-lobby"
    }
}

/// Creates the directory if it is missing, restricting a new directory to its owner.
pub fn prepare(path: &Path) -> Result<(), HomeProblem> {
    if refused(path) {
        return Err(HomeProblem::Refused(path.to_path_buf()));
    }
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(path).map_err(|error| HomeProblem::Io(path.to_path_buf(), error))?;
    restrict_to_owner(path).map_err(|error| HomeProblem::Io(path.to_path_buf(), error))
}

/// Restricts a directory to the current user: on Windows a protected DACL
/// whose one entry grants the current user full control, inherited by
/// everything inside; mode 0700 elsewhere.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        crate::win32::restrict_to_owner(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
}

/// Restricts the directory to its owner and checks that it took: the daemon
/// does this on every start, before it reads or creates the attach secret, so
/// a directory whose access was widened since is narrowed again or refused.
pub fn secure(path: &Path) -> io::Result<()> {
    restrict_to_owner(path)?;
    #[cfg(windows)]
    let owner_only = crate::win32::is_owner_only(path)?;
    #[cfg(unix)]
    let owner_only = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)?.permissions().mode() & 0o077 == 0
    };
    if owner_only {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the directory still grants access to someone other than its owner",
        ))
    }
}

/// A path as an operator should read it: on Windows, without the `\\?\` or
/// `\\?\UNC\` prefix that canonicalization adds.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            return local.to_owned();
        }
    }
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_wins_over_the_platform_folder() {
        let chosen = choose(
            Some(PathBuf::from("/srv/lobby")),
            Some(PathBuf::from("/base")),
        )
        .expect("override");
        assert_eq!(chosen, PathBuf::from("/srv/lobby"));
        let platform = choose(None, Some(PathBuf::from("/base"))).expect("platform");
        assert_eq!(platform, PathBuf::from("/base").join(platform_leaf()));
        assert!(matches!(choose(None, None), Err(HomeProblem::NoBaseFolder)));
    }

    #[test]
    fn a_directory_inside_the_refused_one_is_never_used() {
        for path in [
            "/home/someone/.companion-lobby",
            "/x/.Companion-Lobby",
            "/x/.companion-lobby/inner",
            "/x/.companion-lobby/sub/..",
            "/x/.companion-lobby/./inner/../deeper",
            "/x/.companion-lobby.",
            "/x/.companion-lobby. .",
            "/x/.companion-lobby ",
            "/x/.companion-lobby::$INDEX_ALLOCATION",
        ] {
            assert!(
                matches!(
                    choose(Some(PathBuf::from(path)), None),
                    Err(HomeProblem::Refused(_))
                ),
                "{path}"
            );
        }
        assert!(!refused(Path::new("/x/companion-lobby")));
        assert!(!refused(Path::new("/x/.companion-lobby-other")));
        assert!(!refused(Path::new("/x/.companion-lobby/..")), "that is /x");
        assert!(matches!(
            prepare(Path::new("/nowhere/.companion-lobby/inner")),
            Err(HomeProblem::Refused(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_spellings_of_the_refused_directory_are_refused() {
        for path in [
            r"C:\Users\x\.companion-lobby\sub\..",
            r"C:\Users\x\.COMPANION-LOBBY\",
            r"C:/Users/x/.companion-lobby/inner",
            r"\?\C:\Users\x\.companion-lobby",
        ] {
            assert!(refused(Path::new(path)), "{path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_that_leads_into_the_refused_directory_is_refused() {
        let root = std::env::temp_dir().join(format!(
            "lobby-junction-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        let target = root.join(REFUSED_COMPONENT);
        std::fs::create_dir_all(&target).expect("target");
        let alias = root.join("alias");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&alias)
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run mklink");
        assert!(made.success());
        assert!(
            !names_refused(&lexical(&alias)),
            "only the link leads there"
        );
        assert!(refused(&alias));
        assert!(refused(&alias.join("not-yet-created")));
        assert!(!refused(&root.join("elsewhere")));
        let _ = std::fs::remove_dir(&alias);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn securing_a_directory_leaves_it_to_its_owner_alone() {
        let dir = std::env::temp_dir().join(format!(
            "lobby-home-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        secure(&dir).expect("secure");
        #[cfg(windows)]
        assert!(crate::win32::is_owner_only(&dir).expect("read the DACL"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // Securing again changes nothing and still succeeds.
        secure(&dir).expect("secure again");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefixes_are_stripped_for_display() {
        assert_eq!(display_path(Path::new(r"\\?\C:\data")), r"C:\data");
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share")),
            r"\\server\share"
        );
        assert_eq!(display_path(Path::new(r"C:\data")), r"C:\data");
    }
}
