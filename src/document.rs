//! a document: what ryad knows about one tab apart from its text, which the
//! editor control owns.
//!
//! three revisions per document:
//!   rev        the live text; +1 on every change
//!   saved_rev  the revision whose text is in the user's file (for a new
//!              draft: the empty start). the dot shows while rev != saved_rev
//!   snap_rev   the revision whose copy the committed session holds
//! revisions come from one counter shared with document ids (session
//! `next_id`), so a snapshot name `<id>-<rev>.txt` is never reused, even
//! across restarts.

use std::path::{Path, PathBuf};

use crate::files::{DiskVersion, Encoding, Eol};

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// the file is gone; the tab keeps what it had
    Missing,
    /// the file changed on disk after the text in this tab was based on it
    ChangedOnDisk,
    /// restoring this tab's saved copy failed; the message says why
    RestoreFailed(String),
}

pub struct Doc {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub eol: Eol,
    pub mixed: bool,
    pub disk: Option<DiskVersion>,
    pub rev: u64,
    pub saved_rev: u64,
    pub snap_rev: u64,
    pub snap_name: Option<String>,
    /// length and hash of the saved text in control form, for cheap "back to
    /// the saved text by undo" checks on small documents
    pub saved_len: i32,
    pub saved_hash: u64,
    pub notice: Option<Notice>,
    /// title of a draft: its first non-empty line
    pub draft_title: String,
    /// logical lines, for the gutter width (counted at load, kept up to
    /// date on edits of texts the control lays out quickly)
    pub lines: i32,
    /// the markdown preview is shown for this tab
    pub preview: bool,
}

pub const NEW_DRAFT: &str = "new draft";
const TITLE_CHARS: usize = 28;

impl Doc {
    pub fn new(id: u64, rev: u64) -> Doc {
        Doc {
            id,
            path: None,
            encoding: Encoding::Utf8,
            eol: Eol::Crlf,
            mixed: false,
            disk: None,
            rev,
            saved_rev: rev,
            snap_rev: rev,
            snap_name: None,
            saved_len: 0,
            saved_hash: crate::files::fnv64(b""),
            notice: None,
            draft_title: String::new(),
            lines: 1,
            preview: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// a snapshot is needed when there are unsaved changes and the session
    /// does not hold this revision yet.
    pub fn needs_snapshot(&self) -> bool {
        self.dirty() && self.snap_rev != self.rev
    }

    pub fn title(&self) -> String {
        match &self.path {
            Some(p) => file_name(p),
            None if self.draft_title.is_empty() => NEW_DRAFT.to_string(),
            None => self.draft_title.clone(),
        }
    }

    /// an untouched new draft can be replaced by the next opened file.
    pub fn is_blank(&self, len: i32) -> bool {
        self.path.is_none() && len == 0 && !self.dirty() && self.notice.is_none()
    }
}

/// files that open with the markdown preview on
pub fn is_markdown(p: &Path) -> bool {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).is_some_and(|e| matches!(e.as_str(), "md" | "markdown" | "mdown" | "mkd" | "mdwn"))
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// the first non-empty line, trimmed and cut to a short tab title.
pub fn draft_title(head: &str) -> String {
    let line = head.split(['\r', '\n', '\u{b}', '\u{c}']).map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut t: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        t = t.trim_end().to_string();
        t.push('…');
    }
    t.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(draft_title(""), "");
        assert_eq!(draft_title("\r  \r  список покупок  \rмолоко"), "список покупок");
        assert_eq!(draft_title("дуже довгий перший рядок чернетки, який не влазить"), "дуже довгий перший рядок чер…");
        assert_eq!(draft_title("\tтаб"), "таб");
        let mut d = Doc::new(1, 5);
        assert_eq!(d.title(), NEW_DRAFT);
        assert!(!d.dirty() && d.is_blank(0));
        d.rev = 6;
        assert!(d.dirty() && d.needs_snapshot());
        d.snap_rev = 6;
        assert!(!d.needs_snapshot());
        d.path = Some(PathBuf::from("C:\\x\\нотатки.md"));
        assert_eq!(d.title(), "нотатки.md");
    }
}
