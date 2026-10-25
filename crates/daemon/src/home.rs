//! The data directory.
//!
//! Where the daemon keeps its files:
//!
//! * Windows: the Local AppData known folder, resolved with
//!   `SHGetKnownFolderPath` for the process token and never from environment
//!   variables, followed by `CompanionLobby`; the folder must lie under the
//!   profile root Windows records for the user, or the daemon refuses to
//!   start;
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
//! Whoever creates the directory creates it owner-only in the same call, so
//! it never exists with any other access, and then checks that nothing is in
//! it yet that the lobby did not write. A directory that is not owner-only is
//! restricted only when everything in it is a file the lobby itself writes;
//! anything else is refused rather than re-permissioned, so a mistyped
//! `LOBBY_DATA_DIR` never rewrites the access of a folder the lobby does not
//! own.

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
    /// The base folder does not lie under the profile root Windows records
    /// for the current user.
    Unanchored {
        /// The folder the platform reported.
        folder: PathBuf,
        /// The profile root it should lie under.
        profile: PathBuf,
    },
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
            Self::Unanchored { folder, profile } => write!(
                f,
                "the Local AppData folder resolves to {}, outside the profile at {}; set LOBBY_DATA_DIR",
                display_path(folder),
                display_path(profile)
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
    choose(override_dir.map(PathBuf::from), platform_base)
}

/// The pure decision behind [`resolve`]: the platform folder is asked for
/// only when nothing overrides it.
pub fn choose(
    override_dir: Option<PathBuf>,
    platform_base: impl FnOnce() -> Result<PathBuf, HomeProblem>,
) -> Result<PathBuf, HomeProblem> {
    let path = match override_dir {
        Some(path) => path,
        None => platform_base()?.join(platform_leaf()),
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

/// FOLDERID_LocalAppData through SHGetKnownFolderPath for the process
/// token, accepted only under the profile root Windows records for the
/// user's SID. Windows stores the folder as a path under `%USERPROFILE%`, so
/// the root, which no variable moves, is what anchors it.
#[cfg(windows)]
fn platform_base() -> Result<PathBuf, HomeProblem> {
    let folder = crate::win32::local_app_data().map_err(|_| HomeProblem::NoBaseFolder)?;
    let profile = crate::win32::profile_root().map_err(|_| HomeProblem::NoBaseFolder)?;
    anchored(folder, profile)
}

#[cfg(target_os = "macos")]
fn platform_base() -> Result<PathBuf, HomeProblem> {
    // ~/Library/Application Support
    dirs::data_dir().ok_or(HomeProblem::NoBaseFolder)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_base() -> Result<PathBuf, HomeProblem> {
    // $XDG_STATE_HOME, or ~/.local/state when it is unset.
    dirs::state_dir().ok_or(HomeProblem::NoBaseFolder)
}

/// `folder` when it lies under `profile`, compared the way Windows compares
/// paths: after `.` and `..` are folded, and without case.
#[cfg_attr(not(windows), allow(dead_code))]
fn anchored(folder: PathBuf, profile: PathBuf) -> Result<PathBuf, HomeProblem> {
    let inside = {
        let folder = lexical(&folder);
        let profile = lexical(&profile);
        let mut folder = folder.components();
        profile.components().all(|part| {
            folder.next().is_some_and(|other| {
                part.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&other.as_os_str().to_string_lossy())
            })
        })
    };
    if inside {
        Ok(folder)
    } else {
        Err(HomeProblem::Unanchored { folder, profile })
    }
}

const fn platform_leaf() -> &'static str {
    if cfg!(any(windows, target_os = "macos")) {
        "CompanionLobby"
    } else {
        "companion-lobby"
    }
}

/// Creates the directory if it is missing, owner-only from the call that
/// creates it, then [`secure`]s it. A directory this call created is
/// checked like a narrowed one: anything in it that is not a private lobby
/// file was put there by someone else, and the directory is refused. Nothing
/// is written into the directory before it is known to be the lobby's own.
pub fn prepare(path: &Path) -> Result<(), HomeProblem> {
    prepare_with(path, |_| {})
}

/// [`prepare`], with `after_create` run right after this call created the
/// directory and before it is checked, so a test can stand in for whoever
/// races the creation.
fn prepare_with(path: &Path, after_create: impl FnOnce(&Path)) -> Result<(), HomeProblem> {
    if refused(path) {
        return Err(HomeProblem::Refused(path.to_path_buf()));
    }
    let problem = |error| HomeProblem::Io(path.to_path_buf(), error);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(problem)?;
    }
    if create_private(path).map_err(problem)? {
        after_create(path);
        if let Some(name) = first_entry_not_its_own(path).map_err(problem)? {
            return Err(problem(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{name} appeared in the directory as Companion Lobby created it; remove {name} after checking it, and start again"
                ),
            )));
        }
    }
    secure(path).map_err(problem)
}

/// Creates the directory `path`, whose parent exists, owner-only in the one
/// call that creates it. `false` when something is already there.
fn create_private(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        crate::win32::create_private_dir(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }
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

/// Makes sure the directory belongs to its owner alone, or refuses it. The
/// daemon does this on every start, before it writes anything there, so a
/// directory whose access was widened since is narrowed again or refused.
///
/// An owner-only directory is left as it is. One that is not is restricted
/// only when everything in it is a file the lobby itself writes, owned by the
/// current user and with no access of its own beyond what it inherits from
/// the directory, and checked again afterwards, when on Windows every file
/// must also grant access to the current user alone. A directory holding
/// anything else is refused untouched: restricting it would rewrite the
/// access of a folder the lobby does not own, and a lobby file someone else
/// owns or opened up for themselves while the directory was open may have
/// been planted or read, which narrowing the directory would hide.
pub fn secure(path: &Path) -> io::Result<()> {
    if owner_only(path)? {
        return Ok(());
    }
    only_plain_lobby_files(path)?;
    restrict_to_owner(path)?;
    if !owner_only(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the directory still grants access to someone other than its owner, or another account owns it",
        ));
    }
    // A file another account made between the scan and the narrowing keeps
    // its owner and its own entries through it, so the scan runs again, and
    // then every file must grant access to its owner alone, whatever its
    // entries claim about where they came from.
    match first_entry_not_its_own(path)? {
        None => Ok(()),
        Some(name) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{name} still grants access to someone other than its owner after Companion Lobby narrowed the directory, or appeared while it did; remove {name} after checking it, and start again"
            ),
        )),
    }
}

/// The first entry in an owner-only `path` that the lobby did not write, that
/// another account owns or that carries access of its own, or that grants
/// access to anyone but the current user.
fn first_entry_not_its_own(path: &Path) -> io::Result<Option<String>> {
    if let Some(name) = first_foreign_entry(path)? {
        return Ok(Some(name));
    }
    if let Some(name) = first_entry_with_access_of_its_own(path)? {
        return Ok(Some(name));
    }
    first_entry_not_private(path)
}

/// The first entry in `path` that grants access to anyone but the current
/// user: on Windows, what its DACL actually allows, whatever its entries'
/// flags say.
#[cfg(windows)]
fn first_entry_not_private(path: &Path) -> io::Result<Option<String>> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if !crate::win32::is_private_file(&entry.path())? {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

/// Elsewhere the directory's mode is the narrowing, and the owner check in
/// [`first_entry_with_access_of_its_own`] is the whole rule for a file.
#[cfg(unix)]
fn first_entry_not_private(_path: &Path) -> io::Result<Option<String>> {
    Ok(None)
}

/// Refuses a directory holding anything but lobby files the current user
/// owns with no access of their own.
fn only_plain_lobby_files(path: &Path) -> io::Result<()> {
    if let Some(stranger) = first_foreign_entry(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "the directory is not private to its owner and holds {stranger}, which Companion Lobby did not write; choose an empty directory for {DATA_DIR_ENV}"
            ),
        ));
    }
    if let Some(planted) = first_entry_with_access_of_its_own(path)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "the directory is not private to its owner, and {planted} is owned by another account or carries access entries of its own; Companion Lobby left the directory as it is: remove {planted} after checking it, and start again"
            ),
        ));
    }
    Ok(())
}

fn owner_only(path: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        crate::win32::is_owner_only(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o077 == 0)
    }
}

/// The first entry in `path` that is not one of the files the lobby writes.
fn first_foreign_entry(path: &Path) -> io::Result<Option<String>> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(entry.file_type()?.is_file() && is_lobby_file(&name)) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The first entry in `path` that another account owns, or whose access does
/// not come from the directory alone: on Windows a DACL entry of its own,
/// elsewhere an owner other than the directory's.
fn first_entry_with_access_of_its_own(path: &Path) -> io::Result<Option<String>> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        #[cfg(windows)]
        let plain = crate::win32::has_only_inherited_entries(&entry.path())?;
        #[cfg(unix)]
        let plain = {
            use std::os::unix::fs::MetadataExt;
            entry.metadata()?.uid() == std::fs::metadata(path)?.uid()
        };
        if !plain {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

/// Whether `name` is a file the lobby itself writes in the data directory.
fn is_lobby_file(name: &str) -> bool {
    use crate::journal::is_journal_file;
    use crate::keyring::ATTACH_SECRET_FILE;
    use crate::lock::{INSTANCE_FILE, LOCK_FILE, RECORD_FILE};
    use crate::store::{STATE_FILE, is_temporary_of};
    use crate::trace::{TRACE_FILE, TRACE_PREVIOUS};
    [
        ATTACH_SECRET_FILE,
        LOCK_FILE,
        INSTANCE_FILE,
        RECORD_FILE,
        TRACE_FILE,
        TRACE_PREVIOUS,
        STATE_FILE,
    ]
    .contains(&name)
        || is_journal_file(name)
        || crate::keyring::is_sealed_file(name)
        || crate::keyring::is_rotating_file(name)
        || [ATTACH_SECRET_FILE, RECORD_FILE, STATE_FILE]
            .iter()
            .any(|file| is_temporary_of(name, file))
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
        let chosen = choose(Some(PathBuf::from("/srv/lobby")), || {
            panic!("the platform folder is not asked for")
        })
        .expect("override");
        assert_eq!(chosen, PathBuf::from("/srv/lobby"));
        let platform = choose(None, || Ok(PathBuf::from("/base"))).expect("platform");
        assert_eq!(platform, PathBuf::from("/base").join(platform_leaf()));
        assert!(matches!(
            choose(None, || Err(HomeProblem::NoBaseFolder)),
            Err(HomeProblem::NoBaseFolder)
        ));
    }

    #[test]
    fn a_base_folder_outside_the_profile_root_is_refused() {
        let profile = PathBuf::from("/profiles/sponsor");
        for inside in [
            "/profiles/sponsor/AppData/Local",
            "/Profiles/SPONSOR/AppData/Local",
            "/profiles/sponsor/./AppData/Local",
        ] {
            assert_eq!(
                anchored(PathBuf::from(inside), profile.clone()).expect(inside),
                PathBuf::from(inside)
            );
        }
        for outside in [
            "/elsewhere/AppData/Local",
            "/profiles/sponsor-other/AppData/Local",
            "/profiles/sponsor/../other/AppData/Local",
            "/profiles",
        ] {
            let refused = anchored(PathBuf::from(outside), profile.clone());
            assert!(
                matches!(refused, Err(HomeProblem::Unanchored { .. })),
                "{outside}"
            );
        }
        let problem = choose(None, || {
            anchored(PathBuf::from("/elsewhere"), profile.clone())
        })
        .expect_err("refused");
        assert!(problem.to_string().contains("LOBBY_DATA_DIR"), "{problem}");
    }

    /// The line [`report_the_platform_folder`] prints its answer on.
    #[cfg(windows)]
    const PLATFORM_FOLDER_LINE: &str = "platform folder: ";

    /// The platform folder a child process of this test binary resolves
    /// with `environment` set, `None` when Windows reports none.
    #[cfg(windows)]
    fn platform_folder_with(environment: &[(&str, &OsStr)]) -> Option<PathBuf> {
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "home::tests::report_the_platform_folder",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_remove(DATA_DIR_ENV);
        for (name, value) in environment {
            command.env(name, value);
        }
        let output = command.output().expect("run the test binary");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let answer = stdout
            .lines()
            .find_map(|line| line.split_once(PLATFORM_FOLDER_LINE).map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("no answer in {stdout}"));
        (answer != "none").then(|| PathBuf::from(answer))
    }

    /// Every process resolves the same folder, the shim a host started with a
    /// rewritten environment included: the folder comes from the known folder
    /// for the process token, anchored to the profile root Windows records
    /// for the user, never from the variables that usually mirror them. A
    /// `USERPROFILE` naming a profile-shaped scratch folder, with
    /// `AppData\Local` inside it, does not move the directory, and neither
    /// does one that names nothing.
    #[cfg(windows)]
    #[test]
    fn the_platform_folder_comes_from_the_known_folder_never_the_environment() {
        let here = platform_base().expect("this process has a platform folder");
        let scratch = temp_dir("misleading-profile");
        std::fs::create_dir_all(scratch.join("AppData").join("Local")).expect("scratch profile");
        let misleading = scratch.as_os_str();
        let mirrored = platform_folder_with(&[
            ("LOCALAPPDATA", misleading),
            ("APPDATA", misleading),
            ("HOMEDRIVE", OsStr::new("Q:")),
            ("HOMEPATH", OsStr::new(r"\misleading")),
        ]);
        assert_eq!(
            mirrored.as_ref(),
            Some(&here),
            "the mirroring variables are ignored"
        );
        let profile = platform_folder_with(&[("USERPROFILE", misleading)]);
        assert_eq!(
            profile.as_ref(),
            Some(&here),
            "USERPROFILE does not move it"
        );
        let nowhere = platform_folder_with(&[("USERPROFILE", OsStr::new(r"Q:\nowhere"))]);
        assert_eq!(
            nowhere.as_ref(),
            Some(&here),
            "nor does a profile that is not there"
        );
        assert!(!here.starts_with(&scratch));
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// Run only by the test above, in a child process with a misleading
    /// environment; it resolves the folder and creates nothing.
    #[cfg(windows)]
    #[test]
    #[ignore = "run by the_platform_folder_comes_from_the_known_folder_never_the_environment"]
    fn report_the_platform_folder() {
        match platform_base() {
            Ok(base) => println!("{PLATFORM_FOLDER_LINE}{}", base.display()),
            Err(_) => println!("{PLATFORM_FOLDER_LINE}none"),
        }
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
                    choose(Some(PathBuf::from(path)), || Err(HomeProblem::NoBaseFolder)),
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

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-home-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_shared_directory_holding_only_lobby_files_is_narrowed_to_its_owner() {
        let dir = temp_dir("lobby");
        for name in [
            "attach.secret",
            "daemon.lock",
            "trace.log",
            "daemon.json.42-7.tmp",
            "lobby.json",
            "journal.jsonl",
            "journal.5.jsonl",
        ] {
            std::fs::write(dir.join(name), b"x").expect("file");
        }
        assert!(!owner_only(&dir).expect("read"), "a temp folder is shared");
        secure(&dir).expect("secure");
        assert!(owner_only(&dir).expect("read"));
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

    #[test]
    fn a_shared_directory_holding_other_files_is_refused_untouched() {
        for stranger in [
            "notes.txt",
            "daemon.json.tmp",
            "daemon.json.42.tmp",
            "attach.secret.x.tmp",
            "journal.6.jsonl",
            "lobby.json.old",
        ] {
            let dir = temp_dir("foreign");
            std::fs::write(dir.join("trace.log"), b"x").expect("file");
            std::fs::write(dir.join(stranger), b"x").expect("file");
            let error = secure(&dir).expect_err("refused");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{stranger}");
            assert!(
                !owner_only(&dir).expect("read"),
                "the access of a folder the lobby does not own is not rewritten"
            );
            assert!(matches!(prepare(&dir), Err(HomeProblem::Io(..))));
            let _ = std::fs::remove_dir_all(dir);
        }
        let dir = temp_dir("subfolder");
        std::fs::create_dir(dir.join("trace.log")).expect("a folder named like a lobby file");
        assert!(secure(&dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_shared_directory_holding_a_lobby_file_with_access_of_its_own_is_refused_untouched() {
        let dir = temp_dir("planted");
        std::fs::write(dir.join("trace.log"), b"x").expect("file");
        std::fs::write(dir.join("attach.secret"), b"planted").expect("file");
        let granted = std::process::Command::new("icacls")
            .arg(dir.join("attach.secret"))
            .args(["/grant", "*S-1-1-0:R"])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run icacls");
        assert!(granted.success());
        let error = secure(&dir).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("attach.secret"), "{error}");
        assert!(
            !owner_only(&dir).expect("read"),
            "the directory is left as it is"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_lobby_file_with_a_protected_dacl_is_refused_untouched() {
        let dir = temp_dir("protected");
        std::fs::write(dir.join("trace.log"), b"x").expect("file");
        std::fs::write(dir.join("journal.jsonl"), b"x").expect("file");
        crate::win32::tests::plant_protected_dacl_marked_inherited(&dir.join("journal.jsonl"));
        let error = secure(&dir).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("journal.jsonl"), "{error}");
        assert!(
            !owner_only(&dir).expect("read"),
            "the directory is left as it is"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_directory_is_created_private() {
        let parent = temp_dir("parent");
        let dir = parent.join("nested").join("data");
        let mut checked = false;
        prepare_with(&dir, |created| {
            assert!(
                owner_only(created).expect("read"),
                "private before anything else ran"
            );
            checked = true;
        })
        .expect("prepare");
        assert!(checked, "the directory was created by this call");
        assert!(owner_only(&dir).expect("read"));
        prepare(&dir).expect("prepare again");
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn a_directory_that_holds_a_stranger_as_it_is_created_is_refused() {
        for stranger in ["notes.txt", "attach.secret.x.tmp"] {
            let parent = temp_dir("raced");
            let dir = parent.join("data");
            let refused = prepare_with(&dir, |created| {
                std::fs::write(created.join(stranger), b"planted").expect("plant");
            })
            .expect_err("refused");
            assert!(
                matches!(&refused, HomeProblem::Io(_, error) if error.kind() == io::ErrorKind::PermissionDenied),
                "{refused}"
            );
            assert!(refused.to_string().contains(stranger), "{refused}");
            let _ = std::fs::remove_dir_all(parent);
        }
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
