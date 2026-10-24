//! The journal: attribution.
//!
//! The journal is append-only JSON lines in the data directory, one
//! [`JournalEntry`] per line, written synchronously: an append returns only
//! once its line is on the disk. Every entry has an id of the form
//! `<storeId>:<seq>`, where the store id comes from the state file and the
//! sequence number only ever grows, across rotations and restarts: a daemon
//! that opens the journal continues after the highest number it finds.
//!
//! Entries hold no content and no secrets. Each field is a typed identifier,
//! a validated name, a fixed lobby word or an outcome code, so nothing a
//! sponsor typed beyond a moniker and nothing a service sent can reach it.
//!
//! The live file is `journal.jsonl`. When an entry would take it past
//! [`GENERATION_BYTES`], it becomes `journal.1.jsonl`, the older generations
//! move up by one, and the oldest beyond [`GENERATIONS`] is dropped. Nothing is
//! deleted for its age. A rotation is all or nothing: the live file is set
//! aside first, and when that or any later move fails, every move already
//! made is undone, so a file held open by a reader leaves every generation as
//! it was and the live file grows past its limit until the next rotation.
//! A rotation interrupted by a crash leaves the live file set aside, where it
//! is still read, and the next rotation finishes it.
//!
//! A page of the journal is read newest first and fenced by a cursor that
//! names the store and the highest entry the first page saw, so entries
//! appended meanwhile never shift a later page, and a cursor from another
//! journal is refused rather than misread. A page is read through a
//! [`Reader`], taken from the journal in a moment and used without it, so
//! reading never holds up an append; it reads the generations newest first
//! and stops once it has the page, and reads again when a rotation moved the
//! files while it read.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use lobby_contracts::{
    CompanionId, ConnectionId, Initiator, JournalEntry, JournalKind, Outcome, ServiceName, ToolName,
};
use serde_json::{Map, Value, json};

/// The live file's name.
pub const JOURNAL_FILE: &str = "journal.jsonl";
/// How many files the journal keeps: the live one and the five rotated before it.
pub const GENERATIONS: usize = 6;
/// The live file while a rotation moves it to generation 1.
pub const ROTATING_FILE: &str = "journal.rotating.jsonl";
/// The oldest generation while a rotation drops it.
pub const RETIRING_FILE: &str = "journal.retiring.jsonl";
/// The size at which the live file rotates.
pub const GENERATION_BYTES: u64 = 1024 * 1024;
/// A page's default length.
pub const PAGE_DEFAULT: usize = 50;
/// A page's longest length.
pub const PAGE_MAX: usize = 100;
/// How many times a page is read again when rotations keep moving the files.
const PAGE_ATTEMPTS: usize = 8;
/// How long a page waits for a rotation under way to finish.
const ROTATION_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);

/// The name of generation `n`: 0 is the live file, 1 the one rotated last.
pub fn generation_name(n: usize) -> String {
    if n == 0 {
        JOURNAL_FILE.to_owned()
    } else {
        format!("journal.{n}.jsonl")
    }
}

/// Whether `name` is one of the journal's files.
pub fn is_journal_file(name: &str) -> bool {
    name == ROTATING_FILE || name == RETIRING_FILE || read_order().any(|file| file == name)
}

/// The files that hold entries, newest first: the live file, the live file a
/// rotation set aside and did not finish moving, then every rotated
/// generation.
fn read_order() -> impl Iterator<Item = String> {
    std::iter::once(JOURNAL_FILE.to_owned())
        .chain(std::iter::once(ROTATING_FILE.to_owned()))
        .chain((1..GENERATIONS).map(generation_name))
}

/// What a command or a call asks the journal to record; the journal adds the
/// sequence number and the time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Who caused it.
    pub initiator: Initiator,
    /// The companion it concerns, if any.
    pub companion: Option<CompanionId>,
    /// The connection it went through, if any.
    pub connection: Option<ConnectionId>,
    /// The service it reached, if any.
    pub service: Option<ServiceName>,
    /// The tool it called, if any.
    pub tool: Option<ToolName>,
    /// What kind of entry it is.
    pub kind: JournalKind,
    /// How it ended, or `pending` while it has not.
    pub outcome: Outcome,
}

impl Note {
    /// A command's entry, which goes through no connection.
    pub fn command(
        initiator: Initiator,
        companion: Option<CompanionId>,
        name: &'static str,
        outcome: Outcome,
    ) -> Self {
        Self {
            initiator,
            companion,
            connection: None,
            service: None,
            tool: None,
            kind: JournalKind::Command { name },
            outcome,
        }
    }
}

/// The journal of one install.
#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    store_id: String,
    next_seq: u64,
    generation_bytes: u64,
    /// Counts each rotation's start and its end, so a reader sees an odd
    /// number while one runs and a changed number after one ran.
    rotations: Arc<AtomicU64>,
    #[cfg(test)]
    refuse_from: Option<u64>,
}

/// What reading a page needs, taken from the journal in a moment so the
/// files are read without holding it.
#[derive(Debug, Clone)]
pub struct Reader {
    dir: PathBuf,
    store_id: String,
    high_water: u64,
    rotations: Arc<AtomicU64>,
}

/// A page of entries, newest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The entries, as written.
    pub entries: Vec<Value>,
    /// The highest sequence number this page and the ones after it show.
    pub upper: u64,
    /// The cursor for the next, older page, when there is one.
    pub next: Option<String>,
}

/// Why a page could not be read.
#[derive(Debug)]
pub enum PageProblem {
    /// The cursor does not belong to this journal, or is malformed.
    Cursor,
    /// A journal file could not be read.
    Io(io::Error),
}

impl Journal {
    /// Opens the journal in `dir` for the store `store_id`, continuing after
    /// the highest sequence number any generation holds.
    pub fn open(dir: &Path, store_id: &str) -> io::Result<Self> {
        Self::with_generation_bytes(dir, store_id, GENERATION_BYTES)
    }

    /// [`Journal::open`] with another rotation size.
    pub fn with_generation_bytes(
        dir: &Path,
        store_id: &str,
        generation_bytes: u64,
    ) -> io::Result<Self> {
        let mut highest = 0;
        for name in read_order() {
            for (seq, _) in read_entries(dir, &name)? {
                highest = highest.max(seq);
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            store_id: store_id.to_owned(),
            next_seq: highest + 1,
            generation_bytes,
            rotations: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            refuse_from: None,
        })
    }

    /// The sequence number of the last entry written, 0 when there is none.
    pub fn high_water(&self) -> u64 {
        self.next_seq - 1
    }

    /// The store id entries are named under.
    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    /// Writes `note` as the next entry and returns it once it is on the disk.
    /// When this fails, nothing should proceed that the entry was to
    /// attribute; a number whose line reached the file, even unsynced, is
    /// never handed out again.
    pub fn append(&mut self, note: Note) -> io::Result<JournalEntry> {
        let entry = JournalEntry {
            seq: self.next_seq,
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0),
            initiator: note.initiator,
            companion: note.companion,
            connection: note.connection,
            service: note.service,
            tool: note.tool,
            kind: note.kind,
            outcome: note.outcome,
        };
        #[cfg(test)]
        if self.refuse_from.is_some_and(|from| entry.seq >= from) {
            return Err(io::Error::other("the journal refuses appends in this test"));
        }
        let mut line = encode(&self.store_id, &entry).to_string();
        line.push('\n');
        let live = self.dir.join(JOURNAL_FILE);
        let length = std::fs::metadata(&live).map_or(0, |meta| meta.len());
        if length > 0
            && length + line.len() as u64 > self.generation_bytes
            && let Err(error) = self.rotate()
        {
            crate::trace::line(&format!(
                "the journal could not rotate and grows past its limit: {error}"
            ));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&live)?;
        // A line torn by a crash is ended before the next one starts, so it
        // spoils only itself.
        let length = file.metadata()?.len();
        if length > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        let written = file.write_all(line.as_bytes());
        // Some of the line may be in the file even when the write failed.
        self.next_seq += 1;
        written?;
        file.sync_data()?;
        Ok(entry)
    }

    /// Moves the live file to generation 1, all or nothing, telling readers
    /// that the files moved.
    fn rotate(&self) -> io::Result<()> {
        self.rotations.fetch_add(1, Ordering::AcqRel);
        let rotated = self.move_generations();
        self.rotations.fetch_add(1, Ordering::AcqRel);
        rotated
    }

    fn move_generations(&self) -> io::Result<()> {
        let live = self.dir.join(JOURNAL_FILE);
        let rotating = self.dir.join(ROTATING_FILE);
        if rotating.exists() {
            // An earlier rotation set the live file aside and stopped.
            self.shift_in(&rotating)?;
        }
        // A live file held open by a reader fails here, before anything moved.
        std::fs::rename(&live, &rotating)?;
        if let Err(error) = self.shift_in(&rotating) {
            // When even this fails, the set-aside file is still read and the
            // next rotation finishes moving it.
            let _ = std::fs::rename(&rotating, &live);
            return Err(error);
        }
        Ok(())
    }

    /// Moves every generation up by one and `rotating` into generation 1,
    /// undoing every move already made when one fails. No move replaces a
    /// file: each target was moved away the step before, or never existed.
    fn shift_in(&self, rotating: &Path) -> io::Result<()> {
        let retiring = self.dir.join(RETIRING_FILE);
        match std::fs::remove_file(&retiring) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let path = |n: usize| self.dir.join(generation_name(n));
        let mut moves = vec![(path(GENERATIONS - 1), retiring.clone())];
        moves.extend((2..GENERATIONS).rev().map(|n| (path(n - 1), path(n))));
        moves.push((rotating.to_path_buf(), path(1)));
        let mut made: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (from, to) in moves {
            match std::fs::rename(&from, &to) {
                Ok(()) => made.push((from, to)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    for (from, to) in made.into_iter().rev() {
                        let _ = std::fs::rename(&to, &from);
                    }
                    return Err(error);
                }
            }
        }
        // The oldest generation is dropped only once everything else moved;
        // when it cannot be removed now, the next rotation removes it.
        let _ = std::fs::remove_file(&retiring);
        Ok(())
    }

    /// Reads a page; see [`Reader::page`].
    pub fn page(&self, before: Option<&str>, limit: usize) -> Result<Page, PageProblem> {
        self.reader().page(before, limit)
    }

    /// A reader of the journal as it stands now: its pages end at the
    /// entry written last before this call.
    pub fn reader(&self) -> Reader {
        Reader {
            dir: self.dir.clone(),
            store_id: self.store_id.clone(),
            high_water: self.high_water(),
            rotations: Arc::clone(&self.rotations),
        }
    }

    /// Makes every append from sequence number `from` on fail, as a full or
    /// unwritable disk would.
    #[cfg(test)]
    pub(crate) fn refuse_appends_from(&mut self, from: u64) {
        self.refuse_from = Some(from);
    }
}

impl Reader {
    /// Reads a page of at most `limit` entries, newest first: the first page
    /// when `before` is `None`, otherwise the page after the one that handed
    /// out that cursor. A page read while a rotation moved the files is read
    /// again.
    pub fn page(&self, before: Option<&str>, limit: usize) -> Result<Page, PageProblem> {
        let limit = limit.clamp(1, PAGE_MAX);
        let (upper, below) = match before {
            None => (self.high_water, u64::MAX),
            Some(cursor) => self.parse_cursor(cursor).ok_or(PageProblem::Cursor)?,
        };
        for _ in 0..PAGE_ATTEMPTS {
            let seen = self.rotations.load(Ordering::Acquire);
            if seen % 2 == 1 {
                std::thread::sleep(ROTATION_PAUSE);
                continue;
            }
            let page = self.read(upper, below, limit)?;
            if self.rotations.load(Ordering::Acquire) == seen {
                return Ok(page);
            }
        }
        Err(PageProblem::Io(io::Error::new(
            io::ErrorKind::Interrupted,
            "the journal rotated while it was read",
        )))
    }

    /// Reads generations newest first until more than `limit` entries at or
    /// below `upper` and below `below` are in hand: each generation holds
    /// only entries older than the one before it.
    fn read(&self, upper: u64, below: u64, limit: usize) -> Result<Page, PageProblem> {
        let mut entries: Vec<(u64, Value)> = Vec::new();
        for name in read_order() {
            entries.extend(
                read_entries(&self.dir, &name)
                    .map_err(PageProblem::Io)?
                    .into_iter()
                    .filter(|(seq, _)| *seq <= upper && *seq < below),
            );
            if entries.len() > limit {
                break;
            }
        }
        entries.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        entries.dedup_by_key(|(seq, _)| *seq);
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next = more
            .then(|| entries.last().map(|(seq, _)| self.cursor(upper, *seq)))
            .flatten();
        Ok(Page {
            entries: entries.into_iter().map(|(_, value)| value).collect(),
            upper,
            next,
        })
    }

    fn cursor(&self, upper: u64, below: u64) -> String {
        format!("{}.{upper}.{below}", self.store_id)
    }

    fn parse_cursor(&self, cursor: &str) -> Option<(u64, u64)> {
        let mut parts = cursor.split('.');
        let (store, upper, below) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || store != self.store_id {
            return None;
        }
        let upper = upper.parse::<u64>().ok()?;
        let below = below.parse::<u64>().ok()?;
        (upper <= self.high_water && below <= upper.saturating_add(1)).then_some((upper, below))
    }
}

/// The entries of the journal file `name` that parse, with their sequence
/// numbers.
fn read_entries(dir: &Path, name: &str) -> io::Result<Vec<(u64, Value)>> {
    let text = match std::fs::read(dir.join(name)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| {
            value
                .get("seq")
                .and_then(Value::as_u64)
                .map(|seq| (seq, value))
        })
        .collect())
}

/// The JSON form of an entry: only the fields that apply are present.
pub fn encode(store_id: &str, entry: &JournalEntry) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(format!("{store_id}:{}", entry.seq)));
    object.insert("seq".into(), json!(entry.seq));
    object.insert("at".into(), json!(entry.at_unix_ms));
    match entry.initiator {
        Initiator::Mind { attachment } => {
            object.insert("initiator".into(), json!("mind"));
            object.insert("attachment".into(), json!(attachment.to_string()));
        }
        Initiator::Sponsor => {
            object.insert("initiator".into(), json!("sponsor"));
        }
        Initiator::Lobby => {
            object.insert("initiator".into(), json!("lobby"));
        }
    }
    if let Some(companion) = entry.companion {
        object.insert("companion".into(), json!(companion.to_string()));
    }
    if let Some(connection) = entry.connection {
        object.insert("connection".into(), json!(connection.to_string()));
    }
    if let Some(service) = &entry.service {
        object.insert("service".into(), json!(service.as_str()));
    }
    if let Some(tool) = &entry.tool {
        object.insert("tool".into(), json!(tool.as_str()));
    }
    match entry.kind {
        JournalKind::Invoked => {
            object.insert("kind".into(), json!("invoked"));
        }
        JournalKind::Settled => {
            object.insert("kind".into(), json!("settled"));
        }
        JournalKind::Command { name } => {
            object.insert("kind".into(), json!("command"));
            object.insert("command".into(), json!(name));
        }
    }
    object.insert("outcome".into(), json!(entry.outcome.code()));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_contracts::AttachmentId;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lobby-journal-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    const STORE: &str = "0123456789abcdef0123456789abcdef";

    fn note(name: &'static str) -> Note {
        Note::command(
            Initiator::Sponsor,
            Some(CompanionId::from_u128(5)),
            name,
            Outcome::Done,
        )
    }

    fn sequences(page: &Page) -> Vec<u64> {
        page.entries
            .iter()
            .map(|entry| entry["seq"].as_u64().expect("seq"))
            .collect()
    }

    #[test]
    fn entries_are_attributed_lines_with_store_scoped_ids() {
        let dir = temp_dir("lines");
        let mut journal = Journal::open(&dir, STORE).expect("open");
        assert_eq!(journal.high_water(), 0);
        let first = journal.append(note("create_companion")).expect("append");
        assert_eq!(first.seq, 1);
        let attachment = AttachmentId::from_u128(9);
        journal
            .append(Note::command(
                Initiator::Mind { attachment },
                None,
                "attach",
                Outcome::Pending,
            ))
            .expect("append");
        let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).expect("read");
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"], format!("{STORE}:1"));
        assert_eq!(lines[0]["initiator"], "sponsor");
        assert_eq!(lines[0]["kind"], "command");
        assert_eq!(lines[0]["command"], "create_companion");
        assert_eq!(lines[0]["outcome"], "done");
        assert_eq!(lines[1]["initiator"], "mind");
        assert_eq!(lines[1]["attachment"], attachment.to_string());
        assert_eq!(lines[1]["outcome"], "pending");
        assert!(lines[1].get("companion").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sequence_numbers_stay_monotone_across_rotation_and_reopening() {
        let dir = temp_dir("rotate");
        let mut journal = Journal::with_generation_bytes(&dir, STORE, 600).expect("open");
        let mut last = 0;
        for _ in 0..40 {
            let entry = journal.append(note("rename_companion")).expect("append");
            assert!(entry.seq > last);
            last = entry.seq;
        }
        for n in 0..GENERATIONS {
            let path = dir.join(generation_name(n));
            assert!(path.is_file(), "generation {n} exists");
            assert!(std::fs::metadata(&path).expect("meta").len() <= 600);
        }
        assert!(
            !dir.join(generation_name(GENERATIONS)).exists(),
            "no generation beyond the limit is kept"
        );
        // A rotation that just happened leaves the live file nearly empty;
        // reopening still continues after the highest number.
        drop(journal);
        let mut reopened = Journal::with_generation_bytes(&dir, STORE, 600).expect("reopen");
        assert_eq!(reopened.high_water(), last);
        assert_eq!(
            reopened.append(note("attach")).expect("append").seq,
            last + 1
        );
        // Even with the live file gone, the rotated ones carry the count.
        std::fs::remove_file(dir.join(JOURNAL_FILE)).expect("remove the live file");
        let again = Journal::open(&dir, STORE).expect("reopen");
        assert!(again.high_water() >= last);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every rotated generation's bytes, oldest last.
    fn rotated(dir: &Path) -> Vec<Vec<u8>> {
        (1..GENERATIONS)
            .map(|n| std::fs::read(dir.join(generation_name(n))).expect("generation"))
            .collect()
    }

    /// A journal of 600-byte generations with every generation written.
    fn full_journal(label: &str) -> (PathBuf, Journal) {
        let dir = temp_dir(label);
        let mut journal = Journal::with_generation_bytes(&dir, STORE, 600).expect("open");
        for _ in 0..40 {
            journal.append(note("rename_companion")).expect("append");
        }
        (dir, journal)
    }

    /// Opens `name` in `dir` the way a reader that does not allow it to be
    /// moved does: sharing reads and writes, never deletion.
    #[cfg(windows)]
    fn hold(dir: &Path, name: &str) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        const SHARE_READ_AND_WRITE: u32 = 0x1 | 0x2;
        OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_AND_WRITE)
            .open(dir.join(name))
            .expect("hold")
    }

    #[cfg(windows)]
    #[test]
    fn a_live_file_held_open_leaves_every_generation_as_it_was() {
        let (dir, mut journal) = full_journal("held-live");
        let before = rotated(&dir);
        let reader = hold(&dir, JOURNAL_FILE);
        let start = journal.high_water();
        for _ in 0..6 {
            journal.append(note("attach")).expect("append");
        }
        assert_eq!(rotated(&dir), before, "no generation moved or was lost");
        assert!(!dir.join(ROTATING_FILE).exists());
        assert!(!dir.join(RETIRING_FILE).exists());
        let page = journal.page(None, 6).expect("page");
        assert_eq!(
            sequences(&page),
            ((start + 1)..=(start + 6)).rev().collect::<Vec<_>>(),
            "the live file grew past its limit instead"
        );
        drop(reader);
        let live = std::fs::read(dir.join(JOURNAL_FILE)).expect("live");
        journal.append(note("detach")).expect("append");
        let after = rotated(&dir);
        assert_eq!(after[0], live, "the live file became generation 1");
        assert_eq!(
            after[1..],
            before[..GENERATIONS - 2],
            "the rest moved up by one"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_rotated_generation_held_open_undoes_every_move() {
        let (dir, mut journal) = full_journal("held-rotated");
        let before = rotated(&dir);
        let live = std::fs::read(dir.join(JOURNAL_FILE)).expect("live");
        let reader = hold(&dir, &generation_name(2));
        let start = journal.high_water();
        for _ in 0..4 {
            journal.append(note("attach")).expect("append");
        }
        assert_eq!(rotated(&dir), before, "every move was undone");
        assert!(
            std::fs::read(dir.join(JOURNAL_FILE))
                .expect("live")
                .starts_with(&live),
            "the live file is back in place and only grew"
        );
        assert!(!dir.join(ROTATING_FILE).exists());
        drop(reader);
        journal.append(note("detach")).expect("append");
        let after = rotated(&dir);
        assert_eq!(after[1..], before[..GENERATIONS - 2]);
        assert_eq!(journal.high_water(), start + 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rotation_stopped_by_a_crash_is_still_read_and_then_finished() {
        let (dir, journal) = full_journal("interrupted");
        let last = journal.high_water();
        drop(journal);
        let before = rotated(&dir);
        let set_aside = std::fs::read(dir.join(JOURNAL_FILE)).expect("live");
        // The crash came right after the live file was set aside.
        std::fs::rename(dir.join(JOURNAL_FILE), dir.join(ROTATING_FILE)).expect("set aside");
        let mut journal = Journal::with_generation_bytes(&dir, STORE, 600).expect("reopen");
        assert_eq!(journal.high_water(), last, "the set-aside file is counted");
        assert_eq!(
            sequences(&journal.page(None, 1).expect("page")),
            vec![last],
            "and read"
        );
        while dir.join(ROTATING_FILE).exists() {
            journal.append(note("attach")).expect("append");
        }
        let after = rotated(&dir);
        assert_eq!(
            after[1], set_aside,
            "it was finished into generation 1 first"
        );
        assert_eq!(after[2..], before[..GENERATIONS - 3]);
        assert!(!dir.join(RETIRING_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reader_taken_before_rotations_still_reads_its_own_page() {
        let (dir, mut journal) = full_journal("reader");
        let reader = journal.reader();
        let upper = journal.high_water();
        let before = journal.rotations.load(Ordering::Acquire);
        // Few enough that the reader's page is not yet retired.
        for _ in 0..5 {
            journal.append(note("attach")).expect("append");
        }
        let after = journal.rotations.load(Ordering::Acquire);
        assert!(after > before, "the appends rotated the journal");
        assert_eq!(after % 2, 0, "no rotation is left under way");
        let page = reader.page(None, 5).expect("page");
        assert_eq!(
            sequences(&page),
            ((upper - 4)..=upper).rev().collect::<Vec<_>>()
        );
        assert_eq!(page.upper, upper);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_page_of_recent_entries_reads_no_older_generation() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, journal) = full_journal("newest-first");
        let oldest = generation_name(GENERATIONS - 1);
        // Shared with no one: any read of the oldest generation fails.
        let held = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(dir.join(&oldest))
            .expect("hold");
        let upper = journal.high_water();
        let page = journal.page(None, 3).expect("the newest page");
        assert_eq!(
            sequences(&page),
            ((upper - 2)..=upper).rev().collect::<Vec<_>>()
        );
        let mut next = page.next;
        let mut reached = false;
        while let Some(cursor) = next {
            match journal.page(Some(&cursor), 3) {
                Ok(page) => next = page.next,
                Err(PageProblem::Io(_)) => {
                    reached = true;
                    break;
                }
                Err(PageProblem::Cursor) => panic!("a cursor this journal gave"),
            }
        }
        assert!(
            reached,
            "paging back far enough reads the oldest generation"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_torn_last_line_spoils_only_itself() {
        let dir = temp_dir("torn");
        let mut journal = Journal::open(&dir, STORE).expect("open");
        journal.append(note("attach")).expect("append");
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(JOURNAL_FILE))
            .expect("open");
        file.write_all(b"{\"seq\":99,\"id\":").expect("tear");
        drop(file);
        let mut journal = Journal::open(&dir, STORE).expect("reopen");
        assert_eq!(journal.high_water(), 1, "the torn line is not counted");
        assert_eq!(journal.append(note("detach")).expect("append").seq, 2);
        let page = journal.page(None, 10).expect("page");
        assert_eq!(sequences(&page), vec![2, 1]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pages_are_fenced_by_their_cursor() {
        let dir = temp_dir("pages");
        let mut journal = Journal::with_generation_bytes(&dir, STORE, 700).expect("open");
        for _ in 0..12 {
            journal.append(note("rename_companion")).expect("append");
        }
        let first = journal.page(None, 5).expect("first page");
        assert_eq!(sequences(&first), vec![12, 11, 10, 9, 8]);
        assert_eq!(first.upper, 12);
        let cursor = first.next.clone().expect("more");
        // Entries written meanwhile never shift the next page.
        for _ in 0..3 {
            journal.append(note("attach")).expect("append");
        }
        let second = journal.page(Some(&cursor), 5).expect("second page");
        assert_eq!(sequences(&second), vec![7, 6, 5, 4, 3]);
        let third = journal.page(second.next.as_deref(), 5).expect("third page");
        assert_eq!(sequences(&third), vec![2, 1]);
        assert_eq!(third.next, None);
        assert_eq!(
            sequences(&journal.page(None, 2).expect("fresh")),
            vec![15, 14]
        );
        for foreign in [
            cursor.replacen(STORE, "ffffffffffffffffffffffffffffffff", 1),
            format!("{STORE}.99.3"),
            format!("{STORE}.12"),
            format!("{STORE}.12.3.4"),
            format!("{STORE}.x.3"),
            "anything".to_owned(),
        ] {
            assert!(
                matches!(journal.page(Some(&foreign), 5), Err(PageProblem::Cursor)),
                "{foreign}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
