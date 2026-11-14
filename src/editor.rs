//! the text field: the system rich edit control (msftedit.dll) in plain text
//! mode, one control per tab so every tab keeps its own undo history.
//!
//! units at this boundary: positions are rich edit character positions
//! ("cp"): utf-16 code units of the control text, where every line break is
//! one `\r`. logical lines are rich edit paragraphs (0-based here). visual
//! lines (with wrap on) only exist inside this module.
//!
//! choices measured in research/spikes (docs/architecture.md):
//! - IMF_AUTOFONT (font fallback, needed for emoji) makes loading text that
//!   switches between latin and cyrillic ~40x slower (1 MiB: 5.7 s vs 0.1 s):
//!   large texts load without it (Editor::load).
//! - wrap is EM_SETTARGETDEVICE (0 = wrap at the window, 1 = none): it keeps
//!   text, selection, undo and the modified flag.
//! - "replace all" is EM_SETTEXTEX with ST_KEEPUNDO|ST_SELECTION: one undo step.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::UI::Controls::RichEdit::{
    CFM_COLOR, CFM_FACE, CFM_SIZE, CHARFORMAT2W, CHARRANGE, EM_EXGETSEL, EM_EXLIMITTEXT, EM_EXLINEFROMCHAR, EM_EXSETSEL, EM_FINDTEXTEXW, EM_GETLANGOPTIONS,
    EM_GETOLEINTERFACE, EM_GETSCROLLPOS, EM_GETTEXTEX, EM_GETTEXTLENGTHEX, EM_GETTEXTRANGE, EM_SETBKGNDCOLOR, EM_SETCHARFORMAT, EM_SETEVENTMASK,
    EM_SETLANGOPTIONS, EM_SETSCROLLPOS, EM_SETTARGETDEVICE, EM_SETTEXTEX, EM_SETTEXTMODE, EM_SETUNDOLIMIT, ENM_CHANGE, ENM_DROPFILES, ENM_SELCHANGE,
    FINDTEXTEXW, GETTEXTEX, GETTEXTLENGTHEX, GT_DEFAULT, GTL_NUMCHARS, GTL_PRECISE, ITextDocument, MSFTEDIT_CLASS, SCF_ALL, SETTEXTEX, ST_KEEPUNDO,
    ST_SELECTION, TEXTRANGEW, TM_MULTICODEPAGE, TM_MULTILEVELUNDO, TM_PLAINTEXT, tomParagraph,
};
use windows::Win32::UI::Controls::{
    EM_CANUNDO, EM_EMPTYUNDOBUFFER, EM_GETFIRSTVISIBLELINE, EM_LINEINDEX, EM_POSFROMCHAR, EM_REPLACESEL, EM_SCROLLCARET, EM_SETMODIFY, SetWindowTheme,
};

use windows::Win32::UI::Shell::{DefSubclassProc, DragAcceptFiles, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_NOHIDESEL, ES_WANTRETURN, HMENU, SendMessageW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CHAR, WM_KEYDOWN, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WM_VSCROLL, WS_CHILD, WS_CLIPSIBLINGS, WS_HSCROLL, WS_VSCROLL,
};
use windows::core::{Interface, PCWSTR, w};

use crate::ui::Theme;

/// sent to the parent after anything that may move the visible lines
/// (paint, scroll, wheel, size): the gutter and the status follow.
pub const WM_EDITOR_VIEW: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

/// utf-16 units up to which a loaded text keeps font fallback (see load)
pub const AUTOFONT_LIMIT: usize = 128 * 1024;

const FR_DOWN: usize = 1;
const FR_MATCHCASE: usize = 4;

pub fn load_library() -> bool {
    // SAFETY: a system dll that registers the rich edit window classes.
    unsafe { LoadLibraryW(w!("Msftedit.dll")).is_ok() }
}

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    // SAFETY: messages to our own control; pointer arguments outlive the call.
    unsafe { SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

pub struct Editor {
    pub hwnd: HWND,
    tom: Option<ITextDocument>,
    wrap: bool,
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.tom = None;
        // SAFETY: our own child window, destroyed once; its text and undo
        // history are freed with it.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl Editor {
    pub fn create(parent: HWND, id: u16) -> windows::core::Result<Editor> {
        let style = WINDOW_STYLE(
            WS_CHILD.0
                | WS_CLIPSIBLINGS.0
                | WS_VSCROLL.0
                | WS_HSCROLL.0
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_AUTOHSCROLL as u32
                | ES_NOHIDESEL as u32
                | ES_WANTRETURN as u32,
        );
        // SAFETY: child of our window; the class is registered by msftedit.dll.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                MSFTEDIT_CLASS,
                w!(""),
                style,
                0,
                0,
                100,
                100,
                Some(parent),
                Some(HMENU(id as usize as *mut c_void)),
                GetModuleHandleW(None).ok().map(|m| m.into()),
                None,
            )
        }?;
        // plain text, multi-level undo, unicode. must be sent while empty.
        send(hwnd, EM_SETTEXTMODE, (TM_PLAINTEXT.0 | TM_MULTILEVELUNDO.0 | TM_MULTICODEPAGE.0) as usize, 0);
        // the default limit is 32k characters for typing and pasting
        send(hwnd, EM_EXLIMITTEXT, 0, 0x7fff_fffe);
        send(hwnd, EM_SETUNDOLIMIT, 1000, 0);
        let lang = send(hwnd, EM_GETLANGOPTIONS, 0, 0);
        send(hwnd, EM_SETLANGOPTIONS, 0, lang & !(0x10 | 0x80)); // IMF_AUTOFONTSIZEADJUST, IMF_DUALFONT
        send(hwnd, EM_SETEVENTMASK, 0, (ENM_CHANGE | ENM_SELCHANGE | ENM_DROPFILES) as isize);
        // SAFETY: our own control; the subclass is removed with the window.
        unsafe {
            DragAcceptFiles(hwnd, true);
            let _ = SetWindowSubclass(hwnd, Some(subclass), 1, 0);
        }
        send(hwnd, EM_SETTARGETDEVICE, 0, 1);
        Ok(Editor { hwnd, tom: None, wrap: false })
    }

    fn tom(&mut self) -> Option<&ITextDocument> {
        if self.tom.is_none() {
            let mut p: *mut c_void = std::ptr::null_mut();
            if send(self.hwnd, EM_GETOLEINTERFACE, 0, &mut p as *mut _ as isize) != 0 && !p.is_null() {
                // SAFETY: EM_GETOLEINTERFACE hands out an AddRef'd IRichEditOle.
                let ole = unsafe { windows::core::IUnknown::from_raw(p) };
                self.tom = ole.cast::<ITextDocument>().ok();
            }
        }
        self.tom.as_ref()
    }

    pub fn apply_theme(&self, theme: &Theme, face: &[u16], px: i32) {
        send(self.hwnd, EM_SETBKGNDCOLOR, 0, theme.colorref(theme.raised).0 as isize);
        // a little air between the line numbers and the text
        let margin = (px / 2).max(4) as isize;
        send(self.hwnd, windows::Win32::UI::Controls::EM_SETMARGINS, 0x1, margin); // EC_LEFTMARGIN
        let mut cf = CHARFORMAT2W::default();
        cf.Base.cbSize = size_of::<CHARFORMAT2W>() as u32;
        cf.Base.dwMask = CFM_COLOR | CFM_FACE | CFM_SIZE;
        cf.Base.crTextColor = theme.colorref(theme.ink);
        // rich edit converts twips with the system dpi of the process, not
        // the monitor's: scale back so the font has `px` physical pixels.
        cf.Base.yHeight = px * 1440 / crate::ui::system_dpi() as i32;
        let n = face.len().min(31);
        cf.Base.szFaceName[..n].copy_from_slice(&face[..n]);
        send(self.hwnd, EM_SETCHARFORMAT, SCF_ALL as usize, &cf as *const _ as isize);
        let name = if theme.dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
        // SAFETY: theme class for the scroll bars; unknown names are ignored.
        unsafe {
            let _ = SetWindowTheme(self.hwnd, name, PCWSTR::null());
        }
    }

    /// IMF_AUTOFONT: rich edit picks a fallback font for characters the
    /// editor font lacks (emoji), but loading text that switches between
    /// scripts gets ~40x slower with it (spike: 1 MiB 5.7 s vs 0.1 s).
    fn set_autofont(&self, on: bool) {
        let lang = send(self.hwnd, EM_GETLANGOPTIONS, 0, 0);
        send(self.hwnd, EM_SETLANGOPTIONS, 0, if on { lang | 0x2 } else { lang & !0x2 });
    }

    /// replaces the text without undo (loading a file or a snapshot). texts
    /// up to AUTOFONT_LIMIT load with font fallback; larger ones without it
    /// (emoji in them show as boxes; the text itself is intact), and typing
    /// afterwards has it again.
    pub fn load(&mut self, text: &str) {
        let t = crate::wide(text);
        let small = t.len() <= AUTOFONT_LIMIT;
        self.set_autofont(small);
        let st = SETTEXTEX { flags: 0, codepage: 1200 };
        send(self.hwnd, EM_SETTEXTEX, &st as *const _ as usize, t.as_ptr() as isize);
        if !small {
            self.set_autofont(true);
        }
        send(self.hwnd, EM_EMPTYUNDOBUFFER, 0, 0);
        send(self.hwnd, EM_SETMODIFY, 0, 0);
    }

    /// replaces the whole text as one undo step ("replace all", "reload").
    pub fn replace_all(&self, text: &str) {
        self.set_sel(0, -1);
        let t = crate::wide(text);
        let st = SETTEXTEX { flags: ST_KEEPUNDO | ST_SELECTION, codepage: 1200 };
        send(self.hwnd, EM_SETTEXTEX, &st as *const _ as usize, t.as_ptr() as isize);
    }

    pub fn replace_sel(&self, text: &str) {
        let t = crate::wide(text);
        send(self.hwnd, EM_REPLACESEL, 1, t.as_ptr() as isize);
    }

    /// the whole text, one `\r` per line break.
    pub fn text(&self) -> String {
        let n = self.len() as usize;
        let mut buf = vec![0u16; n + 1];
        let g = GETTEXTEX {
            cb: (buf.len() * 2) as u32,
            flags: GT_DEFAULT,
            codepage: 1200,
            lpDefaultChar: windows::core::PCSTR::null(),
            lpUsedDefChar: std::ptr::null_mut(),
        };
        let got = send(self.hwnd, EM_GETTEXTEX, &g as *const _ as usize, buf.as_mut_ptr() as isize) as usize;
        String::from_utf16_lossy(&buf[..got.min(n)])
    }

    pub fn len(&self) -> i32 {
        let q = GETTEXTLENGTHEX { flags: GTL_PRECISE | GTL_NUMCHARS, codepage: 1200 };
        send(self.hwnd, EM_GETTEXTLENGTHEX, &q as *const _ as usize, 0) as i32
    }

    pub fn range(&self, a: i32, b: i32) -> String {
        if b <= a {
            return String::new();
        }
        let mut buf = vec![0u16; (b - a) as usize + 2];
        let mut tr = TEXTRANGEW { chrg: CHARRANGE { cpMin: a, cpMax: b }, lpstrText: windows::core::PWSTR(buf.as_mut_ptr()) };
        let n = send(self.hwnd, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize) as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }

    pub fn sel(&self) -> (i32, i32) {
        let mut c = CHARRANGE::default();
        send(self.hwnd, EM_EXGETSEL, 0, &mut c as *mut _ as isize);
        (c.cpMin, c.cpMax)
    }

    /// `b = -1` selects to the end.
    pub fn set_sel(&self, a: i32, b: i32) {
        let c = CHARRANGE { cpMin: a, cpMax: b };
        send(self.hwnd, EM_EXSETSEL, 0, &c as *const _ as isize);
    }

    /// select and bring into view, the way a search result is shown.
    pub fn show_range(&self, a: i32, b: i32) {
        self.set_sel(a, b);
        send(self.hwnd, EM_SCROLLCARET, 0, 0);
    }

    pub fn can_undo(&self) -> bool {
        send(self.hwnd, EM_CANUNDO, 0, 0) != 0
    }

    /// the first visible character: the scroll anchor kept in the session
    /// (pixel offsets change with wrap, font and window width).
    pub fn top(&self) -> i32 {
        let line = send(self.hwnd, EM_GETFIRSTVISIBLELINE, 0, 0);
        send(self.hwnd, EM_LINEINDEX, line as usize, 0) as i32
    }

    /// puts the line of `cp` at the top. by pixel position (EM_POSFROMCHAR
    /// lays the text out up to `cp`): EM_LINESCROLL does nothing right after
    /// a load, while rich edit still lays out lazily.
    pub fn scroll_to(&self, cp: i32) {
        let mut now = POINT::default();
        send(self.hwnd, EM_GETSCROLLPOS, 0, &mut now as *mut _ as isize);
        let line = send(self.hwnd, EM_EXLINEFROMCHAR, 0, cp as isize);
        let start = send(self.hwnd, EM_LINEINDEX, line as usize, 0) as i32;
        let mut at = POINT::default();
        send(self.hwnd, EM_POSFROMCHAR, &mut at as *mut _ as usize, start.max(0) as isize);
        // client y of the line + current scroll = its document y; minus the top margin
        let mut first = POINT::default();
        let top_line = send(self.hwnd, EM_GETFIRSTVISIBLELINE, 0, 0);
        let top_cp = send(self.hwnd, EM_LINEINDEX, top_line as usize, 0) as i32;
        send(self.hwnd, EM_POSFROMCHAR, &mut first as *mut _ as usize, top_cp.max(0) as isize);
        let p = POINT { x: 0, y: (now.y + at.y - first.y).max(0) };
        send(self.hwnd, EM_SETSCROLLPOS, 0, &p as *const _ as isize);
    }

    pub fn wrap(&self) -> bool {
        self.wrap
    }

    /// display only: the text, selection, undo and modified flag stay as they
    /// are (checked in the spike). the first visible character stays on top.
    pub fn set_wrap(&mut self, on: bool) {
        let top = self.top();
        send(self.hwnd, EM_SETTARGETDEVICE, 0, if on { 0 } else { 1 });
        self.wrap = on;
        self.scroll_to(top);
    }

    /// 0-based logical line of a position.
    pub fn line_of(&mut self, cp: i32) -> i32 {
        if !self.wrap {
            // without wrap a visual line is a logical line. (a vertical tab
            // also breaks a visual line; docs/architecture.md, limitations.)
            return send(self.hwnd, EM_EXLINEFROMCHAR, 0, cp as isize) as i32;
        }
        match self.tom() {
            // SAFETY: plain tom calls on our own control.
            Some(d) => unsafe { d.Range(cp, cp).and_then(|r| r.GetIndex(tomParagraph.0)).map(|i| i - 1).unwrap_or(0) },
            None => send(self.hwnd, EM_EXLINEFROMCHAR, 0, cp as isize) as i32,
        }
    }

    /// position where 0-based logical line `n` starts (clamped to the end).
    pub fn line_start(&mut self, n: i32) -> i32 {
        if !self.wrap {
            let i = send(self.hwnd, EM_LINEINDEX, n as usize, 0) as i32;
            return if i < 0 { self.len() } else { i };
        }
        let len = self.len();
        match self.tom() {
            // SAFETY: plain tom calls on our own control.
            Some(d) => unsafe {
                d.Range(0, 0)
                    .and_then(|r| {
                        r.SetIndex(tomParagraph.0, n + 1, 0)?;
                        r.GetStart()
                    })
                    .unwrap_or(len)
            },
            None => len,
        }
    }

    pub fn line_count(&mut self) -> i32 {
        let len = self.len();
        self.line_of(len) + 1
    }

    /// visible visual lines: (y in control pixels, position) from the top.
    pub fn visible_lines(&self, height: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        let first = send(self.hwnd, EM_GETFIRSTVISIBLELINE, 0, 0) as i32;
        let len = self.len();
        let mut v = first;
        loop {
            let cp = send(self.hwnd, EM_LINEINDEX, v as usize, 0) as i32;
            if cp < 0 || (v > first && cp == 0) {
                break;
            }
            let mut p = POINT::default();
            send(self.hwnd, EM_POSFROMCHAR, &mut p as *mut _ as usize, cp as isize);
            if p.y > height {
                break;
            }
            out.push((p.y, cp));
            if cp >= len {
                break;
            }
            v += 1;
            if out.len() > 2000 {
                break;
            }
        }
        out
    }

    /// whether `cp` starts a logical line.
    pub fn starts_line(&self, cp: i32) -> bool {
        cp == 0 || self.range(cp - 1, cp) == "\r"
    }

    /// plain-text search with the control's own matcher, from `from`. down
    /// searches to the end, up to the start. returns the found range.
    pub fn find(&self, what: &str, from: i32, down: bool, case: bool) -> Option<(i32, i32)> {
        if what.is_empty() {
            return None;
        }
        let w = crate::wide(what);
        let chrg = if down { CHARRANGE { cpMin: from, cpMax: -1 } } else { CHARRANGE { cpMin: from, cpMax: 0 } };
        let mut ft = FINDTEXTEXW { chrg, lpstrText: PCWSTR(w.as_ptr()), chrgText: CHARRANGE::default() };
        let flags = if down { FR_DOWN } else { 0 } | if case { FR_MATCHCASE } else { 0 };
        let r = send(self.hwnd, EM_FINDTEXTEXW, flags, &mut ft as *mut _ as isize);
        (r >= 0).then_some((ft.chrgText.cpMin, ft.chrgText.cpMax))
    }

    /// every match, front to back, each counted once.
    pub fn find_all(&self, what: &str, case: bool) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some((a, b)) = self.find(what, from, true, case) {
            if b <= a {
                break;
            }
            out.push((a, b));
            from = b;
        }
        out
    }

    pub fn redo(&self) {
        send(self.hwnd, windows::Win32::UI::Controls::RichEdit::EM_REDO, 0, 0);
    }
}

/// keys and characters a plain text editor must not pass on: rich edit's
/// formatting shortcuts (ctrl+b/i/u/l/e/r/j/1/2/5, ctrl+shift+a, ctrl+=…)
/// would format the whole text in plain text mode, and ctrl combinations
/// type control characters. ryad's own shortcuts arrive as menu commands
/// before this (TranslateAccelerator).
fn blocked_key(vk: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
    if !ctrl || alt {
        return false; // altgr (ctrl+alt) types characters
    }
    let allowed = [
        VK_LEFT,
        VK_RIGHT,
        VK_UP,
        VK_DOWN,
        VK_HOME,
        VK_END,
        VK_PRIOR,
        VK_NEXT,
        VK_BACK,
        VK_DELETE,
        VK_INSERT,
        VK_CONTROL,
        VK_SHIFT,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_TAB,
    ];
    let letters_ok = *b"ACVXZY";
    !(allowed.iter().any(|k| k.0 as u32 == vk) || letters_ok.iter().any(|&c| c as u32 == vk))
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN if blocked_key(wparam.0 as u32) => return LRESULT(0),
        // control characters from ctrl+letter; tab, enter and backspace stay
        WM_CHAR if wparam.0 < 0x20 && ![0x08, 0x09, 0x0d].contains(&wparam.0) => return LRESULT(0),
        // getting the focus must not scroll to the caret: switching tabs or
        // windows keeps the view where it was (rich edit would jump)
        windows::Win32::UI::WindowsAndMessaging::WM_SETFOCUS => {
            let mut pos = POINT::default();
            send(hwnd, EM_GETSCROLLPOS, 0, &mut pos as *mut _ as isize);
            // SAFETY: default handling, then the view is put back.
            let r = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            send(hwnd, EM_SETSCROLLPOS, 0, &pos as *const _ as isize);
            return r;
        }
        _ => {}
    }
    // SAFETY: default handling first, then tell the parent the view may
    // have moved.
    let r = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    if matches!(msg, WM_PAINT | WM_VSCROLL | WM_MOUSEWHEEL | WM_SIZE) {
        // SAFETY: our parent window.
        unsafe {
            if let Ok(parent) = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd) {
                windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(parent), WM_EDITOR_VIEW, WPARAM(0), LPARAM(0)).ok();
            }
        }
    }
    r
}
