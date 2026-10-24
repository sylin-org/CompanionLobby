//! The state file: the truth.
//!
//! `lobby.json` in the data directory holds what the lobby remembers across
//! restarts: the store id that names this install's journal, a revision that
//! every commit advances, and the companions, each with its own revision.
//! Live attachments are runtime facts and never reach it.
//!
//! The file is schema 1: camelCase JSON with unknown fields refused. A file
//! that is empty, does not parse, names no schema or another schema, or holds
//! a value its own rules refuse (a duplicate moniker, an identifier of the
//! wrong shape) is refused, and the daemon does not start on it. A refused
//! file is never rewritten: the daemon writes only a schema it reads.
//!
//! Every write is atomic ([`atomic_write`]): the new bytes go to a temporary
//! file with a name unique to the call, are flushed to the disk, and replace
//! the file in one rename. On Windows a rename that meets another handle
//! holding the file is tried again for a bounded time and then fails closed,
//! leaving the old file whole; elsewhere the directory is synced after the
//! rename. [`Store::commit`] is the one way the state changes: it writes the
//! next state and adopts it only when the write landed, so a failed commit
//! leaves both the file and the state in memory as they were.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lobby_contracts::{Companion, CompanionId, DisplayName, Moniker, Sponsor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The state file's name inside the data directory.
pub const STATE_FILE: &str = "lobby.json";
/// The one schema this build reads and writes.
pub const SCHEMA: u64 = 1;
/// How many times a replace is tried while another handle holds the file.
pub const REPLACE_ATTEMPTS: u32 = 40;
/// The pause between those tries.
pub const REPLACE_PAUSE: Duration = Duration::from_millis(25);

/// A companion as the state file keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionRecord {
    /// The companion.
    pub companion: Companion,
    /// Advances on every change to this companion; a guarded change names it.
    pub revision: u64,
}

/// The lobby's durable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// This install's store id: 32 lowercase hex digits, minted once.
    pub store_id: String,
    /// Advances on every commit.
    pub revision: u64,
    /// The companions, oldest first.
    pub companions: Vec<CompanionRecord>,
}

impl State {
    fn fresh() -> Self {
        Self {
            store_id: uuid::Uuid::now_v7().simple().to_string(),
            revision: 0,
            companions: Vec::new(),
        }
    }

    /// The companion with `id`, if any.
    pub fn companion(&self, id: CompanionId) -> Option<&CompanionRecord> {
        self.companions
            .iter()
            .find(|record| record.companion.id == id)
    }
}

/// The file's own form.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileForm {
    schema: u64,
    store_id: String,
    revision: u64,
    companions: Vec<CompanionForm>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompanionForm {
    id: String,
    moniker: String,
    display_name: String,
    revision: u64,
}

/// Why the state file could not be used.
#[derive(Debug)]
pub enum StoreProblem {
    /// The file could not be read or first written.
    Io(io::Error),
    /// The file is not one this build reads; it was left as it is.
    Refused(String),
}

impl std::fmt::Display for StoreProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read {STATE_FILE}: {error}"),
            Self::Refused(why) => write!(
                f,
                "{STATE_FILE} {why}; it was left as it is and the daemon did not start"
            ),
        }
    }
}

impl std::error::Error for StoreProblem {}

/// The state file and the state last committed to it.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    committed: State,
}

impl Store {
    /// Reads `lobby.json` in `dir`, writing a fresh one when there is none.
    pub fn open(dir: &Path) -> Result<Self, StoreProblem> {
        let path = dir.join(STATE_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Self {
                committed: decode(&bytes).map_err(StoreProblem::Refused)?,
                path,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let committed = State::fresh();
                atomic_write(&path, &encode(&committed)).map_err(StoreProblem::Io)?;
                Ok(Self { path, committed })
            }
            Err(error) => Err(StoreProblem::Io(error)),
        }
    }

    /// The state last committed.
    pub fn state(&self) -> &State {
        &self.committed
    }

    /// Writes `next` with the following revision and adopts it once the write
    /// landed. On failure nothing changes: the file still holds the last
    /// commit and so does [`Store::state`].
    pub fn commit(&mut self, mut next: State) -> io::Result<()> {
        next.store_id.clone_from(&self.committed.store_id);
        next.revision = self.committed.revision.saturating_add(1);
        atomic_write(&self.path, &encode(&next))?;
        self.committed = next;
        Ok(())
    }
}

fn encode(state: &State) -> Vec<u8> {
    let form = FileForm {
        schema: SCHEMA,
        store_id: state.store_id.clone(),
        revision: state.revision,
        companions: state
            .companions
            .iter()
            .map(|record| CompanionForm {
                id: record.companion.id.to_string(),
                moniker: record.companion.moniker.as_str().to_owned(),
                display_name: record.companion.display_name.as_str().to_owned(),
                revision: record.revision,
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&form).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Decodes a state file, or says why it is refused.
fn decode(bytes: &[u8]) -> Result<State, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("is empty".to_owned());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "is not JSON this build can read".to_owned())?;
    match value.get("schema") {
        Some(schema) if schema.as_u64() == Some(SCHEMA) => {}
        Some(schema) => {
            return Err(format!(
                "has schema {schema}, and this build reads schema {SCHEMA} only"
            ));
        }
        None => return Err("names no schema".to_owned()),
    }
    let form: FileForm = serde_json::from_value(value)
        .map_err(|error| format!("does not follow schema {SCHEMA}: {error}"))?;
    if form.store_id.len() != 32
        || !form
            .store_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("holds a store id that is not 32 lowercase hex digits".to_owned());
    }
    let mut companions: Vec<CompanionRecord> = Vec::with_capacity(form.companions.len());
    for entry in form.companions {
        let id = CompanionId::parse(&entry.id)
            .ok_or_else(|| "holds a companion id that is not 32 lowercase hex digits".to_owned())?;
        let moniker = Moniker::new(&entry.moniker)
            .map_err(|problem| format!("holds a moniker that is refused: {problem}"))?;
        let display_name = DisplayName::new(&entry.display_name)
            .map_err(|problem| format!("holds a display name that is refused: {problem}"))?;
        if companions
            .iter()
            .any(|known| known.companion.id == id || known.companion.moniker == moniker)
        {
            return Err("holds two companions with one id or one moniker".to_owned());
        }
        if entry.revision == 0 {
            return Err("holds a companion at revision 0".to_owned());
        }
        companions.push(CompanionRecord {
            companion: Companion {
                id,
                moniker,
                display_name,
                sponsor: Sponsor::LOCAL,
            },
            revision: entry.revision,
        });
    }
    Ok(State {
        store_id: form.store_id,
        revision: form.revision,
        companions,
    })
}

/// A temporary name beside `path` that no other call, in this process or
/// another, uses: `<name>.<pid>-<n>.tmp`.
pub fn temporary_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        "{name}.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Whether `name` is a temporary name [`temporary_path`] makes for `file`.
pub fn is_temporary_of(name: &str, file: &str) -> bool {
    name.strip_prefix(file)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(pid, n)| {
            [pid, n]
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

/// Replaces `path` with `bytes` atomically: a reader sees the old file or the
/// new one, whole, never a mix and never a short file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let result = write_new(&temporary, bytes).and_then(|()| replace(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Creates `path`, which must not exist (owner-only off Windows), and writes
/// `bytes` through to the disk.
pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Renames `from` over `to`, trying again while another handle holds `to`,
/// then syncs the directory where the platform can.
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => break,
            Err(error) if attempt < REPLACE_ATTEMPTS && held_by_another_handle(&error) => {
                attempt += 1;
                std::thread::sleep(REPLACE_PAUSE);
            }
            Err(error) => return Err(error),
        }
    }
    sync_directory(to);
    Ok(())
}

/// Whether a rename failed because another handle holds the file open in a
/// way that forbids replacing it (access denied, a sharing or a lock
/// violation). Such a failure passes once that handle closes.
#[cfg(windows)]
fn held_by_another_handle(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(not(windows))]
fn held_by_another_handle(_error: &io::Error) -> bool {
    false
}

/// Syncs the directory holding `path`, so the rename itself is durable. The
/// rename has landed either way; a failed sync is not reported as a failed
/// write.
fn sync_directory(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-store-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn companion(id: u128, moniker: &str) -> CompanionRecord {
        CompanionRecord {
            companion: Companion {
                id: CompanionId::from_u128(id),
                moniker: Moniker::new(moniker).expect("moniker"),
                display_name: DisplayName::new("Someone").expect("name"),
                sponsor: Sponsor::LOCAL,
            },
            revision: 1,
        }
    }

    #[test]
    fn a_new_store_is_written_at_once_and_read_back_after_a_commit() {
        let dir = temp_dir("fresh");
        let mut store = Store::open(&dir).expect("open");
        assert!(dir.join(STATE_FILE).is_file());
        let store_id = store.state().store_id.clone();
        assert_eq!(store_id.len(), 32);
        let mut next = store.state().clone();
        next.companions.push(companion(7, "ada"));
        store.commit(next).expect("commit");
        assert_eq!(store.state().revision, 1);
        let again = Store::open(&dir).expect("reopen");
        assert_eq!(again.state(), store.state());
        assert_eq!(again.state().store_id, store_id);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![STATE_FILE.to_owned()],
            "no temporary file is left"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_this_build_does_not_read_is_refused_and_left_as_it_is() {
        let valid = r#"{"schema":1,"storeId":"0123456789abcdef0123456789abcdef","revision":0,"companions":[]}"#;
        let company = r#"{"id":"0123456789abcdef0123456789abcdef","moniker":"ada","displayName":"Ada","revision":1}"#;
        for (bytes, why) in [
            (String::new(), "is empty"),
            ("  \n".to_owned(), "is empty"),
            ("{".to_owned(), "is not JSON"),
            (
                r#"{"storeId":"0123456789abcdef0123456789abcdef","revision":0,"companions":[]}"#
                    .to_owned(),
                "names no schema",
            ),
            (
                valid.replace("\"schema\":1", "\"schema\":2"),
                "has schema 2",
            ),
            (
                valid.replace("\"schema\":1", "\"schema\":\"1\""),
                "has schema",
            ),
            (
                valid.replace("\"companions\":[]", "\"companions\":[],\"extra\":true"),
                "does not follow schema 1",
            ),
            (
                valid.replace("0123456789abcdef0123", "0123456789ABCDEF0123"),
                "store id",
            ),
            (
                valid.replace("[]", &format!("[{company},{company}]")),
                "two companions",
            ),
            (
                valid.replace("[]", &format!("[{}]", company.replace("ada", "Ada!"))),
                "moniker",
            ),
            (
                valid.replace(
                    "[]",
                    &format!("[{}]", company.replace("\"revision\":1", "\"revision\":0")),
                ),
                "revision 0",
            ),
        ] {
            let dir = temp_dir("refused");
            std::fs::write(dir.join(STATE_FILE), &bytes).expect("write");
            let problem = Store::open(&dir).expect_err("refused");
            let text = problem.to_string();
            assert!(text.contains(why), "{why}: {text}");
            assert!(text.contains("left as it is"), "{text}");
            assert_eq!(
                std::fs::read_to_string(dir.join(STATE_FILE)).expect("still there"),
                bytes,
                "a refused file is never rewritten"
            );
            let _ = std::fs::remove_dir_all(dir);
        }
        let dir = temp_dir("valid");
        std::fs::write(
            dir.join(STATE_FILE),
            valid.replace("[]", &format!("[{company}]")),
        )
        .expect("write");
        let store = Store::open(&dir).expect("a valid file opens");
        assert_eq!(
            store.state().companions[0].companion.moniker.as_str(),
            "ada"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn temporary_names_are_unique_per_call_and_recognized() {
        let path = Path::new("/x/lobby.json");
        let first = temporary_path(path);
        let second = temporary_path(path);
        assert_ne!(first, second);
        for temporary in [&first, &second] {
            let name = temporary.file_name().expect("name").to_string_lossy();
            assert!(is_temporary_of(&name, STATE_FILE), "{name}");
        }
        for stranger in [
            "lobby.json",
            "lobby.json.tmp",
            "lobby.json.12.tmp",
            "lobby.json.x-1.tmp",
            "lobby.json.1-.tmp",
            "lobby.json.1-2-3.tmp",
            "other.json.1-2.tmp",
        ] {
            assert!(!is_temporary_of(stranger, STATE_FILE), "{stranger}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_replace_while_another_handle_holds_the_file_fails_closed_and_then_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let dir = temp_dir("held");
        let mut store = Store::open(&dir).expect("open");
        let before = std::fs::read(dir.join(STATE_FILE)).expect("read");
        // A reader that lets others read but not delete or replace the file.
        let holder = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(dir.join(STATE_FILE))
            .expect("hold the file");
        let mut next = store.state().clone();
        next.companions.push(companion(9, "bea"));
        let started = std::time::Instant::now();
        assert!(
            store.commit(next.clone()).is_err(),
            "the replace fails closed"
        );
        assert!(started.elapsed() >= REPLACE_PAUSE * (REPLACE_ATTEMPTS - 1));
        assert_eq!(std::fs::read(dir.join(STATE_FILE)).expect("read"), before);
        assert_eq!(store.state().revision, 0, "the state rolled back");
        assert!(store.state().companions.is_empty());
        drop(holder);
        store.commit(next).expect("commit once the handle is gone");
        assert_eq!(store.state().companions.len(), 1);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![STATE_FILE.to_owned()],
            "no temporary file is left"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reader_holding_the_file_keeps_the_whole_old_bytes_across_a_replace() {
        use std::io::Read;
        let dir = temp_dir("reader");
        let mut store = Store::open(&dir).expect("open");
        let before = std::fs::read(dir.join(STATE_FILE)).expect("read");
        let mut reader = File::open(dir.join(STATE_FILE)).expect("open a reader");
        let mut next = store.state().clone();
        next.companions.push(companion(9, "bea"));
        store
            .commit(next)
            .expect("the default sharing allows the replace");
        let mut seen = Vec::new();
        reader.read_to_end(&mut seen).expect("read");
        assert_eq!(
            seen, before,
            "the open handle still reads the old file whole"
        );
        let after = std::fs::read(dir.join(STATE_FILE)).expect("read");
        assert!(decode(&after).is_ok());
        drop(reader);
        let _ = std::fs::remove_dir_all(dir);
    }
}
