//! user files: decoding, encoding, line endings, the version on disk and the
//! safe save (temp file next to the target, verified, then ReplaceFileW).
//!
//! units: the editor control keeps one `\r` per line break, so `Decoded::text`
//! is utf-16-compatible rust text with `\r` breaks only. positions are never
//! stored here.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};
use windows::core::PCWSTR;

use crate::wide;

/// the largest file ryad opens. checked from the file size before any buffer
/// is allocated; see docs/measurements.md for what was measured.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Win1251,
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf8Bom => "utf-8 bom",
            Encoding::Utf16Le => "utf-16 le",
            Encoding::Utf16Be => "utf-16 be",
            Encoding::Win1251 => "windows-1251",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf8Bom => "utf-8-bom",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Win1251 => "windows-1251",
        }
    }
    pub fn from_key(s: &str) -> Option<Encoding> {
        [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Win1251].into_iter().find(|e| e.key() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Crlf,
    Lf,
    Cr,
}

impl Eol {
    pub fn name(self) -> &'static str {
        match self {
            Eol::Crlf => "crlf",
            Eol::Lf => "lf",
            Eol::Cr => "cr",
        }
    }
    pub fn from_key(s: &str) -> Option<Eol> {
        [Eol::Crlf, Eol::Lf, Eol::Cr].into_iter().find(|e| e.name() == s)
    }
    fn bytes(self) -> &'static str {
        match self {
            Eol::Crlf => "\r\n",
            Eol::Lf => "\n",
            Eol::Cr => "\r",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Decoded {
    /// text for the control: every line break is a single `\r`.
    pub text: String,
    pub encoding: Encoding,
    /// the most common line ending (crlf when there is none).
    pub eol: Eol,
    /// more than one kind of line ending; saving after edits writes `eol`.
    pub mixed: bool,
}

#[derive(Debug, PartialEq)]
pub enum OpenError {
    TooLarge(u64),
    Binary,
    /// not valid utf-8 and no bom; the user may reopen it as windows-1251.
    NotUtf8,
    /// text the editor control would silently change.
    Unsupported(&'static str),
}

impl OpenError {
    pub fn message(&self) -> String {
        match self {
            OpenError::TooLarge(n) => format!("this file is {:.0} mb. ryad opens text files up to {} mb.", *n as f64 / 1048576.0, MAX_FILE_BYTES / 1048576),
            OpenError::Binary => "this looks like a binary file (it contains zero bytes), so ryad does not open it as text.".into(),
            OpenError::NotUtf8 => "this file is not valid utf-8 and has no byte order mark.".into(),
            OpenError::Unsupported(what) => format!("this file contains {what}, which the editor would change. it was not opened, to keep the file intact."),
        }
    }
}

/// decodes file bytes. `forced` is the manual choice (windows-1251);
/// otherwise a bom decides, then strict utf-8.
pub fn decode(bytes: &[u8], forced: Option<Encoding>) -> Result<Decoded, OpenError> {
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err(OpenError::Unsupported("a utf-32 byte order mark"));
    }
    let (encoding, raw): (Encoding, String) = match forced {
        Some(Encoding::Win1251) => {
            if bytes.contains(&0) {
                return Err(OpenError::Binary);
            }
            (Encoding::Win1251, bytes.iter().map(|&b| cp1251_char(b)).collect())
        }
        _ => {
            if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
                (Encoding::Utf8Bom, utf8(rest)?)
            } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
                (Encoding::Utf16Le, utf16(rest, u16::from_le_bytes)?)
            } else if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
                (Encoding::Utf16Be, utf16(rest, u16::from_be_bytes)?)
            } else {
                (Encoding::Utf8, utf8(bytes)?)
            }
        }
    };
    if raw.contains('\0') {
        return Err(OpenError::Binary);
    }
    if raw.contains('\u{2028}') {
        return Err(OpenError::Unsupported("u+2028 line separators"));
    }
    if raw.contains('\u{2029}') {
        return Err(OpenError::Unsupported("u+2029 paragraph separators"));
    }
    let (text, eol, mixed) = to_control(&raw);
    Ok(Decoded { text, encoding, eol, mixed })
}

fn utf8(bytes: &[u8]) -> Result<String, OpenError> {
    if bytes.contains(&0) {
        return Err(OpenError::Binary);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| OpenError::NotUtf8)
}

fn utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> Result<String, OpenError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(OpenError::Unsupported("an odd number of bytes for utf-16"));
    }
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| f([c[0], c[1]])).collect();
    String::from_utf16(&units).map_err(|_| OpenError::Unsupported("broken utf-16 (an unpaired surrogate)"))
}

/// every crlf, lf and lone cr becomes `\r`. returns the dominant ending and
/// whether endings were mixed. `\r\r\n` is a lone cr followed by a crlf.
pub fn to_control(raw: &str) -> (String, Eol, bool) {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut out = String::with_capacity(raw.len());
    let b = raw.as_bytes();
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                out.push_str(&raw[start..i]);
                out.push('\r');
                if b.get(i + 1) == Some(&b'\n') {
                    crlf += 1;
                    i += 2;
                } else {
                    cr += 1;
                    i += 1;
                }
                start = i;
            }
            b'\n' => {
                out.push_str(&raw[start..i]);
                out.push('\r');
                lf += 1;
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&raw[start..]);
    let kinds = [crlf, lf, cr].iter().filter(|&&n| n > 0).count();
    let eol = if crlf >= lf && crlf >= cr {
        Eol::Crlf
    } else if lf >= cr {
        Eol::Lf
    } else {
        Eol::Cr
    };
    (out, if crlf + lf + cr == 0 { Eol::Crlf } else { eol }, kinds > 1)
}

#[derive(Debug, PartialEq)]
pub struct Unrepresentable {
    pub first: char,
    pub count: usize,
}

/// control text (`\r` breaks) to file bytes, with bom when the encoding has one.
pub fn encode(text: &str, encoding: Encoding, eol: Eol) -> Result<Vec<u8>, Unrepresentable> {
    let joined: String = if eol == Eol::Cr { text.to_string() } else { text.replace('\r', eol.bytes()) };
    Ok(match encoding {
        Encoding::Utf8 => joined.into_bytes(),
        Encoding::Utf8Bom => [&[0xef, 0xbb, 0xbf][..], joined.as_bytes()].concat(),
        Encoding::Utf16Le => [0xfeffu16].into_iter().chain(joined.encode_utf16()).flat_map(u16::to_le_bytes).collect(),
        Encoding::Utf16Be => [0xfeffu16].into_iter().chain(joined.encode_utf16()).flat_map(u16::to_be_bytes).collect(),
        Encoding::Win1251 => {
            let mut out = Vec::with_capacity(joined.len());
            let mut bad: Option<Unrepresentable> = None;
            for c in joined.chars() {
                match cp1251_byte(c) {
                    Some(b) => out.push(b),
                    None => {
                        let e = bad.get_or_insert(Unrepresentable { first: c, count: 0 });
                        e.count += 1;
                    }
                }
            }
            if let Some(e) = bad {
                return Err(e);
            }
            out
        }
    })
}

/// windows-1251, 0x80..=0xff. 0x98 is unassigned; it maps to u+0098 like
/// MultiByteToWideChar does, so every byte round-trips.
const CP1251_HIGH: [u16; 64] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C, 0x040B, 0x040F, //
    0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x0098, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, //
    0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407, //
    0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457, //
];

fn cp1251_char(b: u8) -> char {
    match b {
        0..=0x7f => b as char,
        0x80..=0xbf => char::from_u32(CP1251_HIGH[(b - 0x80) as usize] as u32).unwrap_or('\u{fffd}'),
        _ => char::from_u32(0x0410 + (b - 0xc0) as u32).unwrap_or('\u{fffd}'),
    }
}

fn cp1251_byte(c: char) -> Option<u8> {
    let u = c as u32;
    match u {
        0..=0x7f => Some(u as u8),
        0x0410..=0x044f => Some((u - 0x0410) as u8 + 0xc0),
        _ => CP1251_HIGH.iter().position(|&x| x as u32 == u).map(|i| 0x80 + i as u8),
    }
}

/// what ryad last knew about a user file: size, last write time and a hash
/// of the bytes. the hash decides when only the time changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DiskVersion {
    pub len: u64,
    pub modified: u64,
    pub hash: u64,
}

impl DiskVersion {
    pub fn to_key(self) -> String {
        format!("{},{},{:016x}", self.len, self.modified, self.hash)
    }
    pub fn from_key(s: &str) -> Option<DiskVersion> {
        let mut it = s.split(',');
        let len = it.next()?.parse().ok()?;
        let modified = it.next()?.parse().ok()?;
        let hash = u64::from_str_radix(it.next()?, 16).ok()?;
        Some(DiskVersion { len, modified, hash })
    }
    pub fn of_bytes(bytes: &[u8], modified: u64) -> DiskVersion {
        DiskVersion { len: bytes.len() as u64, modified, hash: fnv64(bytes) }
    }
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn modified_of(meta: &std::fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    meta.last_write_time()
}

/// reads a user file, refusing anything over `MAX_FILE_BYTES` before reading.
pub fn read(path: &Path) -> std::io::Result<Result<(Vec<u8>, DiskVersion), OpenError>> {
    let mut f = std::fs::File::open(path)?;
    let meta = f.metadata()?;
    if meta.is_dir() {
        return Err(std::io::Error::other("this is a folder, not a file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Ok(Err(OpenError::TooLarge(meta.len())));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.read_to_end(&mut bytes)?;
    let v = DiskVersion::of_bytes(&bytes, modified_of(&meta));
    Ok(Ok((bytes, v)))
}

pub enum DiskState {
    Same,
    Changed,
    Missing,
}

/// compares the file on disk with what ryad loaded or last wrote.
pub fn check(path: &Path, known: DiskVersion) -> DiskState {
    let Ok(meta) = std::fs::metadata(path) else { return DiskState::Missing };
    if meta.len() == known.len && modified_of(&meta) == known.modified {
        return DiskState::Same;
    }
    if meta.len() != known.len || meta.len() > MAX_FILE_BYTES {
        return DiskState::Changed;
    }
    match std::fs::read(path) {
        Ok(b) if fnv64(&b) == known.hash => DiskState::Same,
        Ok(_) => DiskState::Changed,
        Err(_) => DiskState::Changed,
    }
}

#[derive(Debug)]
pub enum SaveError {
    NoFolder,
    Denied,
    ReadOnly,
    Locked,
    Full,
    Other(String),
}

impl SaveError {
    pub fn message(&self) -> String {
        match self {
            SaveError::NoFolder => "the folder does not exist anymore.".into(),
            SaveError::Denied => "windows denied access to this file or its folder.".into(),
            SaveError::ReadOnly => "the file is read-only.".into(),
            SaveError::Locked => "another program has the file open and locked.".into(),
            SaveError::Full => "the disk is full.".into(),
            SaveError::Other(s) => s.clone(),
        }
    }
    fn from_io(e: &std::io::Error) -> SaveError {
        match e.raw_os_error() {
            Some(3) => SaveError::NoFolder,           // ERROR_PATH_NOT_FOUND
            Some(5) => SaveError::Denied,             // ERROR_ACCESS_DENIED
            Some(32) | Some(33) => SaveError::Locked, // sharing / lock violation
            Some(39) | Some(112) => SaveError::Full,  // ERROR_HANDLE_DISK_FULL / ERROR_DISK_FULL
            _ => SaveError::Other(e.to_string()),
        }
    }
}

/// test hook: RYAD_TEST_SAVE_FAIL=replace makes the final replace fail, so the
/// "save failed" paths can be checked without a broken disk.
fn fail_replace() -> bool {
    std::env::var("RYAD_TEST_SAVE_FAIL").as_deref() == Ok("replace")
}

/// writes `bytes` to `path` without ever leaving a half-written target: temp
/// file in the same folder, flushed, read back and compared, then
/// ReplaceFileW (keeps the target's attributes and permissions) or, for a new
/// file, MoveFileExW. returns the new version on disk.
pub fn save_atomic(path: &Path, bytes: &[u8]) -> Result<DiskVersion, SaveError> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).ok_or(SaveError::NoFolder)?;
    if !dir.is_dir() {
        return Err(SaveError::NoFolder);
    }
    let exists = path.exists();
    if exists && std::fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false) {
        return Err(SaveError::ReadOnly);
    }
    let tmp = temp_name(path);
    let result = (|| {
        {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| SaveError::from_io(&e))?;
            f.write_all(bytes).map_err(|e| SaveError::from_io(&e))?;
            f.sync_all().map_err(|e| SaveError::from_io(&e))?;
        }
        let back = std::fs::read(&tmp).map_err(|e| SaveError::from_io(&e))?;
        if back != bytes {
            return Err(SaveError::Other("the written file did not read back the same.".into()));
        }
        if fail_replace() {
            return Err(SaveError::Other("test: replace refused (RYAD_TEST_SAVE_FAIL)".into()));
        }
        let (t, s) = (wide(&path.to_string_lossy()), wide(&tmp.to_string_lossy()));
        // SAFETY: valid null-terminated paths that outlive the calls.
        let r = unsafe {
            if exists {
                ReplaceFileW(PCWSTR(t.as_ptr()), PCWSTR(s.as_ptr()), PCWSTR::null(), REPLACEFILE_IGNORE_MERGE_ERRORS, None, None)
            } else {
                MoveFileExW(PCWSTR(s.as_ptr()), PCWSTR(t.as_ptr()), MOVEFILE_WRITE_THROUGH)
            }
        };
        r.map_err(|e| {
            let code = (e.code().0 as u32) & 0xffff;
            match code {
                5 => SaveError::Denied,
                32 | 33 | 1175 | 1176 => SaveError::Locked, // sharing, lock, unable to remove/move replaced
                _ => SaveError::Other(e.message()),
            }
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    let modified = std::fs::metadata(path).map(|m| modified_of(&m)).unwrap_or(0);
    Ok(DiskVersion::of_bytes(bytes, modified))
}

/// replaces `to` with `from` for ryad's own files (session, settings): both
/// in one folder, write-through.
pub fn replace_own(from: &Path, to: &Path) -> std::io::Result<()> {
    let (f, t) = (wide(&from.to_string_lossy()), wide(&to.to_string_lossy()));
    // SAFETY: valid null-terminated paths.
    unsafe { MoveFileExW(PCWSTR(f.as_ptr()), PCWSTR(t.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| std::io::Error::from_raw_os_error((e.code().0 as u32 & 0xffff) as i32))
}

fn temp_name(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    path.with_file_name(format!(".{name}.ryad-{}-{n:x}.tmp", std::process::id()))
}

/// the canonical form of a path for "is this file already open" checks.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    canon(a) == canon(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bytes: &[u8]) {
        let d = decode(bytes, None).unwrap();
        assert!(!d.mixed);
        let back = encode(&d.text, d.encoding, d.eol).unwrap();
        assert_eq!(back, bytes, "round trip of {:?}", String::from_utf8_lossy(bytes));
    }

    #[test]
    fn round_trips_without_edits() {
        roundtrip("один\r\nдва\r\n".as_bytes());
        roundtrip("один\nдва".as_bytes());
        roundtrip("a\rb\r".as_bytes());
        roundtrip(&[&[0xef, 0xbb, 0xbf][..], "з bom\r\nі crlf 😀\r\n".as_bytes()].concat());
        roundtrip(b"");
        let le: Vec<u8> = [0xfeffu16].into_iter().chain("utf-16 le\r\nї 😀".encode_utf16()).flat_map(u16::to_le_bytes).collect();
        roundtrip(&le);
        let be: Vec<u8> = [0xfeffu16].into_iter().chain("utf-16 be\nї".encode_utf16()).flat_map(u16::to_be_bytes).collect();
        roundtrip(&be);
        let long = "слово ".repeat(200_000) + "\r\n";
        roundtrip(long.as_bytes());
    }

    #[test]
    fn detects_encodings() {
        assert_eq!(decode(b"abc", None).unwrap().encoding, Encoding::Utf8);
        assert_eq!(decode(&[0xef, 0xbb, 0xbf, b'a'], None).unwrap().encoding, Encoding::Utf8Bom);
        assert_eq!(decode(&[0xff, 0xfe, b'a', 0], None).unwrap().encoding, Encoding::Utf16Le);
        assert_eq!(decode(&[0xfe, 0xff, 0, b'a'], None).unwrap().encoding, Encoding::Utf16Be);
        assert_eq!(decode(&[0xff, 0xfe, 0, 0], None), Err(OpenError::Unsupported("a utf-32 byte order mark")));
    }

    #[test]
    fn line_endings() {
        let d = decode(b"a\r\nb\nc\rd", None).unwrap();
        assert_eq!(d.text, "a\rb\rc\rd");
        assert!(d.mixed);
        assert_eq!(d.eol, Eol::Crlf);
        // cr cr lf: the rich edit control deletes this sequence (spike); here it
        // is a lone cr and a crlf, two line breaks.
        let d = decode(b"a\r\r\nb", None).unwrap();
        assert_eq!(d.text, "a\r\rb");
        assert!(d.mixed);
        let d = decode(b"a\nb\nc\r\nd", None).unwrap();
        assert_eq!(d.eol, Eol::Lf);
        // no line ending at all: crlf for new lines, nothing mixed
        let d = decode(b"one line", None).unwrap();
        assert_eq!((d.eol, d.mixed), (Eol::Crlf, false));
        // mixed, after edits: normalised to the dominant ending
        assert_eq!(encode("a\rb\rc\rd", Encoding::Utf8, Eol::Crlf).unwrap(), b"a\r\nb\r\nc\r\nd");
    }

    #[test]
    fn refuses_binary_and_unsupported() {
        assert_eq!(decode(b"a\0b", None), Err(OpenError::Binary));
        assert_eq!(decode(&[0xff, 0xfe, b'a', 0, 0, 0], None), Err(OpenError::Binary));
        assert_eq!(decode("a\u{2028}b".as_bytes(), None), Err(OpenError::Unsupported("u+2028 line separators")));
        assert_eq!(decode(&[0xc3, 0x28], None), Err(OpenError::NotUtf8));
        assert!(matches!(decode(&[0xff, 0xfe, 0x00], None), Err(OpenError::Unsupported(_))));
    }

    #[test]
    fn windows_1251() {
        let bytes: Vec<u8> = (1u8..=255).filter(|&b| b != b'\r' && b != b'\n').collect();
        let d = decode(&bytes, Some(Encoding::Win1251)).unwrap();
        assert_eq!(encode(&d.text, Encoding::Win1251, d.eol).unwrap(), bytes);
        let d = decode(&[0xcf, 0xf0, 0xe8, 0xe2, 0xb3, 0xf2, 0x20, 0xa5, 0xb4, 0xaa, 0xba, 0xaf, 0xbf], Some(Encoding::Win1251)).unwrap();
        assert_eq!(d.text, "Привіт ҐґЄєЇї");
        assert_eq!(encode("ї 😀 ї 😀", Encoding::Win1251, Eol::Crlf), Err(Unrepresentable { first: '😀', count: 2 }));
    }

    #[test]
    fn versions() {
        let v = DiskVersion::of_bytes(b"abc", 133);
        assert_eq!(DiskVersion::from_key(&v.to_key()), Some(v));
    }

    #[test]
    fn safe_save_and_check() {
        let dir = std::env::temp_dir().join(format!("ryad-test-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("файл без розширення");
        let v1 = save_atomic(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        assert!(matches!(check(&p, v1), DiskState::Same));
        let v2 = save_atomic(&p, b"two, longer").unwrap();
        assert!(matches!(check(&p, v1), DiskState::Changed));
        assert!(matches!(check(&p, v2), DiskState::Same));
        // no temp files left behind
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // missing folder
        assert!(matches!(save_atomic(&dir.join("nope").join("x.txt"), b"x"), Err(SaveError::NoFolder)));
        // read-only target stays untouched
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&p, perm.clone()).unwrap();
        assert!(matches!(save_atomic(&p, b"three"), Err(SaveError::ReadOnly)));
        assert_eq!(std::fs::read(&p).unwrap(), b"two, longer");
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        std::fs::set_permissions(&p, perm).unwrap();
        // a file held open without share-delete cannot be replaced; the
        // original stays and the temp file is removed
        {
            use std::os::windows::fs::OpenOptionsExt;
            let _lock = std::fs::OpenOptions::new().read(true).share_mode(1).open(&p).unwrap(); // FILE_SHARE_READ only
            assert!(save_atomic(&p, b"four").is_err());
        }
        assert_eq!(std::fs::read(&p).unwrap(), b"two, longer");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert!(matches!(check(&dir.join("gone"), v2), DiskState::Missing));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
