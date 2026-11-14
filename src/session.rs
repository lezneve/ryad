//! the session: which tabs are open, their unsaved text and where the caret
//! was, so that closing ryad (or losing the process) loses nothing that was
//! committed.
//!
//! layout of the session folder (`%LOCALAPPDATA%\ryad\session\`):
//!   session.ini      the committed index (versioned text, checksum last line)
//!   session.bak      the index before that, kept until the next commit
//!   <id>-<rev>.txt   immutable snapshots of unsaved text (utf-8, `\r` breaks)
//!   lock             held open by the one process that owns the session
//!
//! a commit writes the new snapshots first (temp, flush, rename), then the
//! index (temp, flush, ReplaceFileW with session.bak as backup). only after
//! that are snapshots that neither index refers to deleted. one writer
//! thread does all of this in order; the ui thread only hands it jobs.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::files::{DiskVersion, Encoding, Eol, fnv64};

pub const FORMAT: &str = "ryad session 1";
pub const INDEX: &str = "session.ini";
pub const BACKUP: &str = "session.bak";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// an open tab
    Tab,
    /// closed with "keep as draft": listed in the drafts menu
    Kept,
    /// a recently closed tab ctrl+shift+t can bring back (clean files only)
    Closed,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Tab => "tab",
            Kind::Kept => "kept",
            Kind::Closed => "closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub id: u64,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub eol: Eol,
    pub mixed: bool,
    /// the user file's version this text started from (or was last saved as)
    pub disk: Option<DiskVersion>,
    /// unsaved text, when there is any
    pub snapshot: Option<Snapshot>,
    pub wrap: bool,
    /// markdown preview shown next to the text
    pub preview: bool,
    /// selection in control units (utf-16, one unit per line break)
    pub sel: (i32, i32),
    /// first visible character, the scroll anchor
    pub top: i32,
    pub title: String,
    /// seconds since 1970, for the drafts menu
    pub when: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub name: String,
    pub len: u64,
    pub hash: u64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Index {
    pub generation: u64,
    pub next_id: u64,
    /// position of the active tab among the `Kind::Tab` entries
    pub active: usize,
    pub entries: Vec<Entry>,
}

impl Index {
    pub fn to_text(&self) -> String {
        let mut s = format!("{FORMAT}\ngeneration = {}\nnext_id = {}\nactive = {}\n", self.generation, self.next_id, self.active);
        for e in &self.entries {
            s.push_str(&format!("[{}]\nid = {}\n", e.kind.key(), e.id));
            if let Some(p) = &e.path {
                s.push_str(&format!("path = {}\n", p.display()));
            }
            s.push_str(&format!("encoding = {}\neol = {}\nmixed = {}\n", e.encoding.key(), e.eol.name(), e.mixed as u8));
            if let Some(d) = e.disk {
                s.push_str(&format!("disk = {}\n", d.to_key()));
            }
            if let Some(sn) = &e.snapshot {
                s.push_str(&format!("snapshot = {},{},{:016x}\n", sn.name, sn.len, sn.hash));
            }
            s.push_str(&format!("preview = {}\n", e.preview as u8));
            s.push_str(&format!(
                "wrap = {}\nsel = {},{}\ntop = {}\nwhen = {}\ntitle = {}\n",
                e.wrap as u8,
                e.sel.0,
                e.sel.1,
                e.top,
                e.when,
                one_line(&e.title)
            ));
        }
        let sum = fnv64(s.as_bytes());
        s.push_str(&format!("end = {sum:016x}\n"));
        s
    }

    /// parses an index; any damage (wrong format, missing checksum, a
    /// truncated write) makes the whole index invalid.
    pub fn parse(text: &str) -> Result<Index, String> {
        let body_end = text.rfind("end = ").ok_or("no checksum line")?;
        let (body, tail) = text.split_at(body_end);
        let sum = tail.trim_end().strip_prefix("end = ").and_then(|h| u64::from_str_radix(h, 16).ok()).ok_or("bad checksum line")?;
        if fnv64(body.as_bytes()) != sum {
            return Err("checksum does not match".into());
        }
        let mut lines = body.lines();
        if lines.next() != Some(FORMAT) {
            return Err("unknown session format".into());
        }
        let mut idx = Index::default();
        let mut cur: Option<Entry> = None;
        for line in lines {
            if let Some(kind) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if let Some(e) = cur.take() {
                    idx.entries.push(e);
                }
                let kind = match kind {
                    "tab" => Kind::Tab,
                    "kept" => Kind::Kept,
                    "closed" => Kind::Closed,
                    other => return Err(format!("unknown section {other}")),
                };
                cur = Some(Entry {
                    kind,
                    id: 0,
                    path: None,
                    encoding: Encoding::Utf8,
                    eol: Eol::Crlf,
                    mixed: false,
                    disk: None,
                    snapshot: None,
                    wrap: false,
                    preview: false,
                    sel: (0, 0),
                    top: 0,
                    title: String::new(),
                    when: 0,
                });
                continue;
            }
            let (k, v) = line.split_once(" = ").ok_or_else(|| format!("bad line: {line}"))?;
            let bad = || format!("bad value for {k}");
            match &mut cur {
                None => match k {
                    "generation" => idx.generation = v.parse().map_err(|_| bad())?,
                    "next_id" => idx.next_id = v.parse().map_err(|_| bad())?,
                    "active" => idx.active = v.parse().map_err(|_| bad())?,
                    _ => {}
                },
                Some(e) => match k {
                    "id" => e.id = v.parse().map_err(|_| bad())?,
                    "path" => e.path = Some(PathBuf::from(v)),
                    "encoding" => e.encoding = Encoding::from_key(v).ok_or_else(bad)?,
                    "eol" => e.eol = Eol::from_key(v).ok_or_else(bad)?,
                    "mixed" => e.mixed = v == "1",
                    "disk" => e.disk = Some(DiskVersion::from_key(v).ok_or_else(bad)?),
                    "snapshot" => {
                        let mut it = v.split(',');
                        let name = it.next().filter(|n| valid_snapshot_name(n)).ok_or_else(bad)?.to_string();
                        let len = it.next().and_then(|x| x.parse().ok()).ok_or_else(bad)?;
                        let hash = it.next().and_then(|x| u64::from_str_radix(x, 16).ok()).ok_or_else(bad)?;
                        e.snapshot = Some(Snapshot { name, len, hash });
                    }
                    "wrap" => e.wrap = v == "1",
                    "preview" => e.preview = v == "1",
                    "sel" => {
                        let (a, b) = v.split_once(',').ok_or_else(bad)?;
                        e.sel = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
                    }
                    "top" => e.top = v.parse().map_err(|_| bad())?,
                    "when" => e.when = v.parse().map_err(|_| bad())?,
                    "title" => e.title = v.to_string(),
                    _ => {}
                },
            }
        }
        if let Some(e) = cur.take() {
            idx.entries.push(e);
        }
        Ok(idx)
    }

    pub fn snapshot_names(&self) -> HashSet<String> {
        self.entries.iter().filter_map(|e| e.snapshot.as_ref().map(|s| s.name.clone())).collect()
    }
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// `<id>-<rev>.txt`, nothing else: names from the index are used as file
/// names, so a damaged index must not point outside the folder.
fn valid_snapshot_name(n: &str) -> bool {
    n.strip_suffix(".txt").is_some_and(|b| {
        let mut it = b.split('-');
        let ok = |x: Option<&str>| x.is_some_and(|x| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()));
        ok(it.next()) && ok(it.next()) && it.next().is_none()
    })
}

pub fn snapshot_name(id: u64, rev: u64) -> String {
    format!("{id}-{rev}.txt")
}

/// what restoring found: the index used, which file it came from, and
/// problems with single entries (the rest still loads).
pub struct Restored {
    pub index: Index,
    pub from_backup: bool,
    pub problems: Vec<String>,
}

/// reads the committed index, falling back to the backup when the index is
/// damaged. snapshot problems are reported per entry; such an entry keeps
/// its place without text.
pub fn restore(dir: &Path) -> Option<Restored> {
    let mut problems = Vec::new();
    let mut from_backup = false;
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok().map(|t| Index::parse(&t));
    let index = match read(INDEX) {
        Some(Ok(i)) => i,
        other => {
            if let Some(Err(e)) = other {
                problems.push(format!("the session index was damaged ({e}); the previous one was used."));
            }
            match read(BACKUP) {
                Some(Ok(i)) => {
                    from_backup = true;
                    i
                }
                Some(Err(e)) => {
                    problems.push(format!("the backup session index was damaged too ({e})."));
                    return Some(Restored { index: Index::default(), from_backup, problems });
                }
                None if problems.is_empty() => return None,
                None => return Some(Restored { index: Index::default(), from_backup, problems }),
            }
        }
    };
    Some(Restored { index, from_backup, problems })
}

/// reads one snapshot and checks it against the index.
pub fn read_snapshot(dir: &Path, s: &Snapshot) -> Result<String, String> {
    let bytes = std::fs::read(dir.join(&s.name)).map_err(|e| format!("its saved copy could not be read ({e})"))?;
    if bytes.len() as u64 != s.len || fnv64(&bytes) != s.hash {
        return Err("its saved copy is incomplete".into());
    }
    String::from_utf8(bytes).map_err(|_| "its saved copy is damaged".into())
}

// ---- writer -----------------------------------------------------------------

pub struct Commit {
    pub generation: u64,
    pub index: Index,
    /// snapshots that may not exist yet, by name
    pub snapshots: Vec<(String, Arc<String>)>,
    /// documents whose saved copies must disappear from every recovery path
    /// (discarded changes), even from the backup index
    pub purge: Vec<u64>,
}

pub enum Job {
    Commit(Commit),
    /// reply once everything before this job is on disk
    Flush(Sender<Result<u64, String>>),
}

/// the result of the newest commit the writer finished, read by the ui
/// thread after the writer's notification message.
#[derive(Default)]
pub struct Status {
    pub done: u64,
    pub error: Option<String>,
}

pub struct Writer {
    tx: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    pub status: Arc<Mutex<Status>>,
}

impl Writer {
    /// `notify` runs on the writer thread after each commit (posts a message
    /// to the window).
    pub fn start(dir: PathBuf, notify: impl Fn() + Send + 'static) -> Writer {
        let (tx, rx) = channel::<Job>();
        let status = Arc::new(Mutex::new(Status::default()));
        let st = status.clone();
        let thread = std::thread::Builder::new().name("ryad session writer".into()).spawn(move || run(dir, rx, st, notify)).expect("start the session writer");
        Writer { tx: Some(tx), thread: Some(thread), status }
    }

    pub fn send(&self, job: Job) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(job);
        }
    }

    /// waits until every job sent so far is written.
    pub fn flush(&self) -> Result<u64, String> {
        let (tx, rx) = channel();
        self.send(Job::Flush(tx));
        rx.recv().unwrap_or_else(|_| Err("the session writer stopped".into()))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(dir: PathBuf, rx: Receiver<Job>, status: Arc<Mutex<Status>>, notify: impl Fn()) {
    let mut written: HashSet<String> = HashSet::new();
    let mut last_error: Option<String> = None;
    while let Ok(first) = rx.recv() {
        // merge commits that queued up while the last one was written: the
        // newest index wins, every snapshot it needs is kept, purges add up.
        let mut jobs = vec![first];
        while let Ok(j) = rx.try_recv() {
            jobs.push(j);
        }
        let mut pending: Option<Commit> = None;
        let mut flushes = Vec::new();
        for j in jobs {
            match j {
                Job::Commit(c) => {
                    pending = Some(match pending.take() {
                        None => c,
                        Some(mut old) => {
                            old.snapshots.extend(c.snapshots);
                            old.purge.extend(c.purge);
                            Commit { generation: c.generation, index: c.index, snapshots: old.snapshots, purge: old.purge }
                        }
                    });
                }
                Job::Flush(reply) => flushes.push((reply, pending.as_ref().map(|c| c.generation))),
            }
        }
        if let Some(c) = pending {
            let t = std::time::Instant::now();
            let generation = c.generation;
            match commit(&dir, c, &mut written) {
                Ok(bytes) => {
                    last_error = None;
                    crate::trace!("session: generation {generation} committed, {bytes} bytes of snapshots, {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
                    let mut s = status.lock().unwrap();
                    s.done = generation;
                    s.error = None;
                }
                Err(e) => {
                    crate::trace!("session: generation {generation} failed: {e}");
                    last_error = Some(e.clone());
                    status.lock().unwrap().error = Some(e);
                }
            }
            notify();
        }
        for (reply, _) in flushes {
            let done = status.lock().unwrap().done;
            let _ = reply.send(match &last_error {
                Some(e) => Err(e.clone()),
                None => Ok(done),
            });
        }
    }
}

/// test hook: RYAD_TEST_FAULT=after-snapshots|after-index[@n] ends the
/// process in the middle of the n-th commit of this run (default: the
/// first), like a killed process or a power cut would.
fn fault(step: &str) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COMMITS: AtomicU32 = AtomicU32::new(0);
    let Ok(v) = std::env::var("RYAD_TEST_FAULT") else { return };
    let (name, nth) = v.split_once('@').map(|(a, b)| (a.to_string(), b.parse().unwrap_or(1))).unwrap_or((v.clone(), 1));
    if step == "after-snapshots" {
        COMMITS.fetch_add(1, Ordering::Relaxed);
    }
    if name == step && COMMITS.load(Ordering::Relaxed) == nth {
        crate::trace!("session: test fault {step} in commit {nth}, exiting");
        std::process::exit(91);
    }
}

/// io errors read as part of a lowercase sentence: "access is denied".
fn lower(e: &std::io::Error) -> String {
    let s = e.to_string();
    let s = s.split(" (os error").next().unwrap_or(&s).trim_end_matches('.');
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    crate::files::replace_own(&tmp, &dir.join(name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn commit(dir: &Path, c: Commit, written: &mut HashSet<String>) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("its folder could not be created ({})", lower(&e)))?;
    let needed = c.index.snapshot_names();
    let purge: HashSet<u64> = c.purge.iter().copied().collect();
    let mut bytes = 0;
    let texts: BTreeMap<String, Arc<String>> = c.snapshots.into_iter().collect();
    for name in &needed {
        if written.contains(name) && dir.join(name).exists() {
            continue;
        }
        if let Some(text) = texts.get(name) {
            write_new(dir, name, text.as_bytes()).map_err(|e| format!("a copy could not be written ({})", lower(&e)))?;
            bytes += text.len();
            written.insert(name.clone());
        } else if !dir.join(name).exists() {
            return Err(format!("internal: snapshot {name} is missing"));
        }
    }
    fault("after-snapshots");

    let text = c.index.to_text();
    let tmp = dir.join("session.new");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("the session index could not be written ({})", lower(&e)))?;
        f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("the session index could not be written ({})", lower(&e)))?;
    }
    let index = dir.join(INDEX);
    let backup = dir.join(BACKUP);
    if index.exists() {
        let (i, n, b) = (crate::wide(&index.to_string_lossy()), crate::wide(&tmp.to_string_lossy()), crate::wide(&backup.to_string_lossy()));
        // SAFETY: valid null-terminated paths in our own folder.
        unsafe {
            windows::Win32::Storage::FileSystem::ReplaceFileW(
                windows::core::PCWSTR(i.as_ptr()),
                windows::core::PCWSTR(n.as_ptr()),
                windows::core::PCWSTR(b.as_ptr()),
                windows::Win32::Storage::FileSystem::REPLACEFILE_IGNORE_MERGE_ERRORS,
                None,
                None,
            )
        }
        .map_err(|e| format!("the session index could not be replaced ({})", e.message()))?;
    } else {
        crate::files::replace_own(&tmp, &index).map_err(|e| format!("the session index could not be written ({})", lower(&e)))?;
    }
    fault("after-index");

    // a purge must not survive in the backup either: the backup becomes a
    // copy of the new index.
    if !purge.is_empty() {
        write_new(dir, BACKUP, text.as_bytes()).map_err(|e| format!("the session backup could not be written ({e})"))?;
    }
    // delete what neither index refers to (and, for purged documents, all of
    // their copies).
    let mut keep = needed;
    if let Ok(t) = std::fs::read_to_string(&backup)
        && let Ok(b) = Index::parse(&t)
    {
        keep.extend(b.snapshot_names());
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for f in rd.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let id_of = |n: &str| n.split('-').next().and_then(|x| x.parse::<u64>().ok());
            let stale_tmp = name.ends_with(".txt.tmp");
            let purged = valid_snapshot_name(&name) && id_of(&name).is_some_and(|id| purge.contains(&id));
            if stale_tmp || purged || (valid_snapshot_name(&name) && !keep.contains(&name)) {
                let _ = std::fs::remove_file(f.path());
                written.remove(&name);
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, kind: Kind, snap: Option<(&str, &str)>) -> Entry {
        Entry {
            kind,
            id,
            path: if id.is_multiple_of(2) { Some(PathBuf::from(format!("C:\\тест\\файл {id}.txt"))) } else { None },
            encoding: Encoding::Utf8Bom,
            eol: Eol::Lf,
            mixed: id == 3,
            disk: Some(DiskVersion { len: 10, modified: 20, hash: 30 }),
            snapshot: snap.map(|(n, t)| Snapshot { name: n.into(), len: t.len() as u64, hash: fnv64(t.as_bytes()) }),
            wrap: true,
            preview: id == 1,
            sel: (3, 7),
            top: 2,
            title: "чернетка\tз табом".into(),
            when: 1_700_000_000,
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ryad-test-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn index_round_trip_and_damage() {
        let idx = Index {
            generation: 4,
            next_id: 9,
            active: 1,
            entries: vec![entry(1, Kind::Tab, Some(("1-5.txt", "x"))), entry(2, Kind::Kept, None), entry(4, Kind::Closed, None)],
        };
        let text = idx.to_text();
        let mut back = Index::parse(&text).unwrap();
        // control characters in titles become spaces; everything else round-trips
        for (b, i) in back.entries.iter_mut().zip(&idx.entries) {
            assert_eq!(b.title, "чернетка з табом");
            b.title = i.title.clone();
        }
        assert_eq!(back, idx);
        // truncated write
        assert!(Index::parse(&text[..text.len() / 2]).is_err());
        // one changed byte
        assert!(Index::parse(&text.replace("generation = 4", "generation = 5")).is_err());
        // a path-like snapshot name is rejected
        let evil = Index { entries: vec![entry(1, Kind::Tab, Some(("..\\x.txt", "x")))], ..Default::default() }.to_text();
        assert!(Index::parse(&evil).is_err());
    }

    fn commit_of(generation: u64, entries: Vec<Entry>, snaps: &[(&str, &str)], purge: Vec<u64>) -> Commit {
        Commit {
            generation,
            index: Index { generation, next_id: 10, active: 0, entries },
            snapshots: snaps.iter().map(|(n, t)| (n.to_string(), Arc::new(t.to_string()))).collect(),
            purge,
        }
    }

    #[test]
    fn commits_keep_the_previous_generation_and_clean_up() {
        let dir = tmpdir("commit");
        let mut w = HashSet::new();
        commit(&dir, commit_of(1, vec![entry(1, Kind::Tab, Some(("1-1.txt", "один")))], &[("1-1.txt", "один")], vec![]), &mut w).unwrap();
        commit(&dir, commit_of(2, vec![entry(1, Kind::Tab, Some(("1-2.txt", "два")))], &[("1-2.txt", "два")], vec![]), &mut w).unwrap();
        // the backup (generation 1) still needs 1-1.txt
        assert!(dir.join("1-1.txt").exists() && dir.join("1-2.txt").exists());
        commit(&dir, commit_of(3, vec![entry(1, Kind::Tab, Some(("1-3.txt", "три")))], &[("1-3.txt", "три")], vec![]), &mut w).unwrap();
        assert!(!dir.join("1-1.txt").exists());
        let r = restore(&dir).unwrap();
        assert_eq!(r.index.generation, 3);
        assert_eq!(read_snapshot(&dir, r.index.entries[0].snapshot.as_ref().unwrap()).unwrap(), "три");
        // damaged index: the backup is used
        std::fs::write(dir.join(INDEX), "ryad session 1\ngarbage").unwrap();
        let r = restore(&dir).unwrap();
        assert!(r.from_backup && r.index.generation == 2 && !r.problems.is_empty());
        assert_eq!(read_snapshot(&dir, r.index.entries[0].snapshot.as_ref().unwrap()).unwrap(), "два");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn purge_removes_every_copy() {
        let dir = tmpdir("purge");
        let mut w = HashSet::new();
        commit(&dir, commit_of(1, vec![entry(1, Kind::Tab, Some(("1-1.txt", "секрет"))), entry(3, Kind::Tab, None)], &[("1-1.txt", "секрет")], vec![]), &mut w)
            .unwrap();
        commit(
            &dir,
            commit_of(2, vec![entry(1, Kind::Tab, Some(("1-2.txt", "секрет 2"))), entry(3, Kind::Tab, None)], &[("1-2.txt", "секрет 2")], vec![]),
            &mut w,
        )
        .unwrap();
        // discard document 1: neither index may lead back to its text
        commit(&dir, commit_of(3, vec![entry(3, Kind::Tab, None)], &[], vec![1]), &mut w).unwrap();
        assert!(!dir.join("1-1.txt").exists() && !dir.join("1-2.txt").exists());
        let b = Index::parse(&std::fs::read_to_string(dir.join(BACKUP)).unwrap()).unwrap();
        assert!(b.entries.iter().all(|e| e.id != 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writer_merges_and_flushes_in_order() {
        let dir = tmpdir("writer");
        let w = Writer::start(dir.clone(), || {});
        for g in 1..=20u64 {
            let name = snapshot_name(1, g);
            let text = format!("ревізія {g}");
            w.send(Job::Commit(commit_of(g, vec![entry(1, Kind::Tab, Some((&name, &text)))], &[(&name, &text)], vec![])));
        }
        assert_eq!(w.flush(), Ok(20));
        let r = restore(&dir).unwrap();
        assert_eq!(read_snapshot(&dir, r.index.entries[0].snapshot.as_ref().unwrap()).unwrap(), "ревізія 20");
        drop(w);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn interrupted_commit_leaves_the_last_good_generation() {
        // the snapshot step finished, the index step did not: the previous
        // index and its snapshots are still complete.
        let dir = tmpdir("interrupt");
        let mut w = HashSet::new();
        commit(&dir, commit_of(1, vec![entry(1, Kind::Tab, Some(("1-1.txt", "старе")))], &[("1-1.txt", "старе")], vec![]), &mut w).unwrap();
        write_new(&dir, "1-2.txt", "нове".as_bytes()).unwrap();
        std::fs::write(dir.join("session.new"), "ryad session 1\nhalf").unwrap();
        let r = restore(&dir).unwrap();
        assert_eq!(r.index.generation, 1);
        assert_eq!(read_snapshot(&dir, r.index.entries[0].snapshot.as_ref().unwrap()).unwrap(), "старе");
        // the next commit removes the orphan
        commit(&dir, commit_of(2, vec![entry(1, Kind::Tab, Some(("1-3.txt", "ще")))], &[("1-3.txt", "ще")], vec![]), &mut w).unwrap();
        assert!(!dir.join("1-2.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
