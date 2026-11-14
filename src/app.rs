//! the main window: start-up, the message loop, command routing, tabs and
//! the glue between the editor controls, user files and the session.
//!
//! threads: this ui thread owns every window and control and the live text
//! (inside the rich edit controls). the session writer thread only gets
//! finished snapshots and indexes (session.rs) and answers with a message.
//! user files are read and written here, synchronously (sizes and times in
//! docs/measurements.md).

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, GetDC, HBRUSH, HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MonitorFromRect, ReleaseDC, ScreenToClient, UpdateWindow,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::RichEdit::{ENDROPFILES, SELCHANGE};
use windows::Win32::UI::Controls::{
    NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, NMTTDISPINFOW, TOOLTIPS_CLASSW, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_NEWTOOLRECTW, TTM_POP, TTM_SETMAXTIPWIDTH,
    TTN_GETDISPINFOW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW, WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::dialogs::{self, CANCEL, Icon};
use crate::document::{self, Doc, Notice};
use crate::editor::{self, Editor, WM_EDITOR_VIEW};
use crate::files::{self, DiskState, Encoding, Eol, OpenError};
use crate::find::{self, Mode, Panel};
use crate::session::{self, Entry, Index, Job, Kind, Writer};
use crate::settings::{self, Settings};
use crate::tabs::{self, Hit, Strip, TabInfo};
use crate::ui::{self, Align, Fonts, Paint, Theme, px, rect};
use crate::{APP_NAME, trace, wide};

// ---- commands (menu and keys call the same code) ------------------------------
const CMD_NEW: u16 = 100;
const CMD_OPEN: u16 = 101;
const CMD_SAVE: u16 = 102;
const CMD_SAVE_AS: u16 = 103;
const CMD_CLOSE_TAB: u16 = 104;
const CMD_REOPEN_CLOSED: u16 = 105;
const CMD_EXIT: u16 = 106;
const CMD_REOPEN_1251: u16 = 107;
const CMD_DELETE_DRAFTS: u16 = 108;
const CMD_UNDO: u16 = 110;
const CMD_REDO: u16 = 111;
const CMD_CUT: u16 = 112;
const CMD_COPY: u16 = 113;
const CMD_PASTE: u16 = 114;
const CMD_SELECT_ALL: u16 = 115;
const CMD_FIND: u16 = 120;
const CMD_FIND_NEXT: u16 = 121;
const CMD_FIND_PREV: u16 = 122;
const CMD_REPLACE: u16 = 123;
const CMD_GOTO: u16 = 124;
const CMD_WRAP: u16 = 130;
const CMD_NEXT_TAB: u16 = 131;
const CMD_PREV_TAB: u16 = 132;
const CMD_TAB_LIST: u16 = 133;
const CMD_PREVIEW: u16 = 134;
const CMD_ENC_BASE: u16 = 140; // + encoding index
const CMD_EOL_BASE: u16 = 150; // + eol index
const CMD_BANNER_A: u16 = 160;
const CMD_BANNER_B: u16 = 161;
const CMD_TAB_BASE: u16 = 1000; // + tab index
const CMD_DRAFT_BASE: u16 = 3000; // + kept draft index

const ENCODINGS: [Encoding; 5] = [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Win1251];
const EOLS: [Eol; 3] = [Eol::Crlf, Eol::Lf, Eol::Cr];

const TIMER_COMMIT: usize = 1;
const TIMER_RETRY: usize = 2;
const TIMER_PREVIEW: usize = 3;
const TIMER_SCROLL: usize = 4;
/// quiet time after typing before the preview is drawn again
const PREVIEW_DELAY: u32 = 250;
/// quiet time after the last change before the session is written
const COMMIT_IDLE: u32 = 750;
/// longest time typing may go on without a commit
const COMMIT_MAX: Duration = Duration::from_secs(5);

const WM_SESSION_DONE: u32 = WM_APP + 1;
const WM_RESTORE_SCROLL: u32 = WM_APP + 3;
const EN_CHANGE: u32 = 0x0300;
const EN_SELCHANGE: u32 = 0x0702;
const EN_DROPFILES: u32 = 0x0703;
const BN_CLICKED: u32 = 0;

/// documents up to this many characters get the cheap "undone back to the
/// saved text" check on every change.
const SAVEPOINT_LIMIT: i32 = 2_000_000;

/// bytes of one line above which a file opens with wrap on
const LONG_LINE: usize = 20_000;

struct Tab {
    doc: Doc,
    ed: Editor,
    /// first visible character to restore once the tab is shown: rich edit
    /// scrolls to the caret when it becomes visible and gets the focus
    pending_top: Option<i32>,
}

enum Closed {
    /// a clean file tab; reopened from disk
    File(Entry),
    /// a draft kept from the close question; reopened from the session
    Kept(u64),
}

pub struct App {
    hwnd: HWND,
    strip_hwnd: HWND,
    status_hwnd: HWND,
    gutter_hwnd: HWND,
    banner_hwnd: HWND,
    banner_buttons: [HWND; 2],
    tooltip: HWND,
    find: Panel,
    tabs: Vec<Tab>,
    active: usize,
    kept: Vec<Entry>,
    closed: Vec<Closed>,
    next_id: u64,
    session_dir: PathBuf,
    settings_path: PathBuf,
    writer: Writer,
    generation: u64,
    pending: Option<Instant>,
    /// discarded documents: (id, generation first sent in). kept and sent
    /// again until a commit of that generation or later has succeeded.
    purge: Vec<(u64, u64)>,
    session_error: Option<String>,
    restore_problems: Vec<String>,
    theme: Theme,
    fonts: Fonts,
    dpi: u32,
    strip: Strip,
    /// ryad's own menu bar row above the tabs
    menubar: crate::menubar::Bar,
    menubar_hwnd: HWND,
    edit_brush: HBRUSH,
    loading: bool,
    closing: bool,
    window_active: bool,
    last_focus: HWND,
    /// (doc id, rev, cp, 0-based logical line): saves tom lookups while scrolling
    line_cache: (u64, u64, i32, i32),
    view: (i32, i32, i32),
    status_text: [String; 4],
    /// snapshots the last queued index refers to (name, size, hash)
    known_snaps: Vec<session::Snapshot>,
    /// where the active editor sits (client coordinates)
    editor_rc: RECT,
    gutter_w: i32,
    /// the markdown preview at the right and the splitter before it
    preview: crate::preview::Preview,
    split_hwnd: HWND,
    /// share of the space right of the gutter that the text takes
    split: f32,
    splitting: bool,
    /// what the preview shows now: (doc id, rev, width, colours)
    preview_key: (u64, u64, i32, u64),
    accent: [f32; 3],
    /// a scroll restore waiting for rich edit's layout: (doc id, tries)
    scroll_retry: Option<(u64, isize)>,
}

static mut APP: *mut App = std::ptr::null_mut();

/// the one App of this ui thread. windows calls back into it re-entrantly
/// (a SendMessage to a control can come back as a notification), the same
/// pattern as kadr's window state: handlers keep borrows short.
fn app<'a>() -> Option<&'a mut App> {
    // SAFETY: set once before the first message and cleared after the loop;
    // only the ui thread touches it.
    unsafe { (*std::ptr::addr_of!(APP)).as_mut() }
}

pub fn run(data: PathBuf, paths: Vec<PathBuf>, owner: crate::ipc::Owner) -> i32 {
    if !editor::load_library() {
        dialogs::error(None, "ryad could not load the windows text control (msftedit.dll).");
        return 1;
    }
    let session_dir = data.join("session");
    let settings_path = data.join("settings.ini");
    let settings = settings::load(&settings_path);
    register_classes(&owner.class);

    // SAFETY: our own registered class; the window is shown after restore.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_ACCEPTFILES,
            PCWSTR(owner.class.as_ptr()),
            w!("ryad"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            GetModuleHandleW(None).ok().map(|m| m.into()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        dialogs::error(None, "ryad could not create its window.");
        return 1;
    };
    // SAFETY: plain query on our window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let theme = if ui::high_contrast() { ui::system_theme() } else { Theme::current() };
    // SAFETY: screen dc for font queries, released right away.
    let fonts = unsafe {
        let dc = GetDC(None);
        let f = ui::make_fonts(dc, dpi);
        ReleaseDC(None, dc);
        f
    };
    let notify_hwnd = hwnd.0 as isize;
    let writer = Writer::start(session_dir.clone(), move || {
        // SAFETY: posting to our window from the writer thread is allowed;
        // a closed window just drops the message.
        unsafe {
            let _ = PostMessageW(Some(HWND(notify_hwnd as *mut c_void)), WM_SESSION_DONE, WPARAM(0), LPARAM(0));
        }
    });
    let make = |class: PCWSTR, style: u32| -> HWND {
        // SAFETY: children of our window.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_CLIPSIBLINGS.0 | style),
                0,
                0,
                10,
                10,
                Some(hwnd),
                None,
                GetModuleHandleW(None).ok().map(|m| m.into()),
                None,
            )
            .unwrap_or_default()
        }
    };
    let strip_hwnd = make(w!("ryad.tabs"), WS_VISIBLE.0);
    let menubar_hwnd = make(w!("ryad.menubar"), WS_VISIBLE.0);
    let status_hwnd = make(w!("ryad.status"), WS_VISIBLE.0);
    let gutter_hwnd = make(w!("ryad.gutter"), WS_VISIBLE.0);
    let banner_hwnd = make(w!("ryad.banner"), 0);
    let bb = |id: u16| -> HWND {
        // SAFETY: buttons inside our banner.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
                0,
                0,
                10,
                10,
                Some(banner_hwnd),
                Some(HMENU(id as usize as *mut c_void)),
                GetModuleHandleW(None).ok().map(|m| m.into()),
                None,
            )
            .unwrap_or_default()
        }
    };
    let banner_buttons = [bb(CMD_BANNER_A), bb(CMD_BANNER_B)];
    let find = Panel::create(hwnd, w!("ryad.find"));
    let preview = crate::preview::Preview::create(hwnd);
    let split_hwnd = make(w!("ryad.split"), 0);
    let tooltip = make_tooltip(strip_hwnd);

    let app_box = Box::new(App {
        hwnd,
        strip_hwnd,
        status_hwnd,
        gutter_hwnd,
        banner_hwnd,
        banner_buttons,
        tooltip,
        find,
        tabs: Vec::new(),
        active: 0,
        kept: Vec::new(),
        closed: Vec::new(),
        next_id: 1,
        session_dir,
        settings_path,
        writer,
        generation: 0,
        pending: None,
        purge: Vec::new(),
        session_error: None,
        restore_problems: Vec::new(),
        theme,
        fonts,
        dpi,
        strip: Strip::default(),
        menubar: crate::menubar::Bar::new(build_menu()),
        menubar_hwnd,
        // SAFETY: plain brush, deleted in drop.
        edit_brush: unsafe { CreateSolidBrush(ui::colorref(theme.cap)) },
        loading: false,
        closing: false,
        window_active: true,
        last_focus: HWND::default(),
        line_cache: (0, 0, -1, 0),
        view: (-1, -1, -1),
        status_text: Default::default(),
        known_snaps: Vec::new(),
        editor_rc: rect(0, 0, 100, 100),
        gutter_w: 0,
        preview,
        split_hwnd,
        split: settings.split.unwrap_or(0.5),
        splitting: false,
        preview_key: (0, 0, 0, 0),
        accent: ui::accent(theme.ground, theme.dark),
        scroll_retry: None,
    });
    // SAFETY: published before the first message that needs it.
    unsafe { APP = Box::into_raw(app_box) };
    let a = app().expect("app");
    a.find.set_fonts(&a.fonts);
    a.preview.apply_theme(&a.theme, a.p(24));
    a.set_banner_fonts();
    ui::apply_title_bar(hwnd, &a.theme);
    // SAFETY: our own window.
    unsafe {
        DragAcceptFiles(hwnd, true);
    }
    // the window gets its final size before any text is loaded: rich edit
    // lays the whole text out again on every size change (~1 s per 10 mib).
    place_window(hwnd, &settings);
    a.layout();
    a.restore();
    if !paths.is_empty() {
        a.open_paths(&paths);
    }
    a.activate(a.active.min(a.tabs.len().saturating_sub(1)));
    trace!("ready for input, {} tab(s), {}", a.tabs.len(), crate::memory_summary());
    if !a.restore_problems.is_empty() {
        let text = a.restore_problems.join("\n");
        dialogs::problem(hwnd, "some of the last session could not be restored", &text);
    }
    let accel = accelerators();
    let mut msg = MSG::default();
    // SAFETY: the standard loop; accelerators first so ryad's keys work in
    // every control, then dialog navigation inside the find panel.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if let Some(a) = app()
                && a.menu_key(&msg)
            {
                continue;
            }
            if TranslateAcceleratorW(hwnd, accel, &msg) != 0 {
                continue;
            }
            // dialog keys (tab, enter, esc) only while the focus is inside
            // the find panel; for other windows IsDialogMessage would walk
            // a tree that is not a dialog.
            if let Some(a) = app()
                && a.find.open
                && IsChild(a.find.hwnd, GetFocus()).as_bool()
                && IsDialogMessageW(a.find.hwnd, &msg).as_bool()
            {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyAcceleratorTable(accel);
    }
    // SAFETY: the window is gone; nothing calls back any more.
    let a = unsafe { Box::from_raw(APP) };
    unsafe { APP = std::ptr::null_mut() };
    drop(a); // editors, writer thread (joined), fonts, brushes
    drop(owner);
    trace!("exit");
    0
}

fn place_window(hwnd: HWND, s: &Settings) {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    // SAFETY: our own window.
    unsafe {
        let _ = GetWindowPlacement(hwnd, &mut wp);
        if let Some(r) = s.window {
            let rc = rect(r[0], r[1], r[2], r[3]);
            // only when that place is still on some monitor
            let mon = MonitorFromRect(&rc, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONULL);
            if !mon.is_invalid() {
                wp.rcNormalPosition = rc;
            }
        } else {
            // first start: a calm default size, centred on the nearest monitor
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut mi =
                windows::Win32::Graphics::Gdi::MONITORINFO { cbSize: size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32, ..Default::default() };
            let mon = MonitorFromRect(&wp.rcNormalPosition, MONITOR_DEFAULTTONEAREST);
            let _ = windows::Win32::Graphics::Gdi::GetMonitorInfoW(mon, &mut mi);
            let (w, h) = (px(dpi, 960).min(mi.rcWork.right - mi.rcWork.left), px(dpi, 680).min(mi.rcWork.bottom - mi.rcWork.top));
            let (l, t) = (mi.rcWork.left + (mi.rcWork.right - mi.rcWork.left - w) / 2, mi.rcWork.top + (mi.rcWork.bottom - mi.rcWork.top - h) / 3);
            wp.rcNormalPosition = rect(l, t, l + w, t + h);
        }
        wp.showCmd = if s.maximized { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 };
        let _ = SetWindowPlacement(hwnd, &wp);
        let _ = UpdateWindow(hwnd);
    }
}

fn register_classes(main_class: &[u16]) {
    let wc = |name: PCWSTR, proc_: WNDPROC, cursor: PCWSTR| {
        // SAFETY: class registration with static strings and procs.
        unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: proc_,
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                lpszClassName: name,
                hCursor: LoadCursorW(None, cursor).unwrap_or_default(),
                hIcon: LoadIconW(
                    GetModuleHandleW(None).ok().map(|m| m.into()),
                    PCWSTR(std::ptr::without_provenance(1)), /* MAKEINTRESOURCE(1): the app icon */
                )
                .unwrap_or_default(),
                style: CS_DBLCLKS,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
    };
    wc(PCWSTR(main_class.as_ptr()), Some(main_proc), IDC_ARROW);
    wc(w!("ryad.tabs"), Some(strip_proc), IDC_ARROW);
    wc(w!("ryad.status"), Some(status_proc), IDC_ARROW);
    wc(w!("ryad.gutter"), Some(gutter_proc), IDC_ARROW);
    wc(w!("ryad.banner"), Some(banner_proc), IDC_ARROW);
    wc(w!("ryad.find"), Some(find_proc), IDC_ARROW);
    wc(w!("ryad.split"), Some(split_proc), IDC_SIZEWE);
    wc(w!("ryad.menubar"), Some(menubar_proc), IDC_ARROW);
}

fn make_tooltip(strip: HWND) -> HWND {
    // SAFETY: a tooltip owned by the strip; one tool covering the strip whose
    // text is asked for on demand (TTN_GETDISPINFO).
    unsafe {
        let tip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(strip),
            None,
            GetModuleHandleW(None).ok().map(|m| m.into()),
            None,
        )
        .unwrap_or_default();
        let mut ti = TTTOOLINFOW {
            cbSize: size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_SUBCLASS,
            hwnd: strip,
            uId: 1,
            lpszText: windows::core::PWSTR(-1isize as *mut u16), // LPSTR_TEXTCALLBACKW
            ..Default::default()
        };
        let _ = GetClientRect(strip, &mut ti.rect);
        SendMessageW(tip, TTM_ADDTOOLW, None, Some(LPARAM(&ti as *const _ as isize)));
        SendMessageW(tip, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(800)));
        tip
    }
}

fn build_menu() -> HMENU {
    // SAFETY: menus owned by the window (destroyed with it).
    unsafe {
        let bar = CreateMenu().unwrap_or_default();
        let add = |m: HMENU, id: u16, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(m, MF_STRING, id as usize, PCWSTR(t.as_ptr()));
        };
        let sep = |m: HMENU| {
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        };
        let sub = |bar: HMENU, m: HMENU, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(bar, MF_POPUP, m.0 as usize, PCWSTR(t.as_ptr()));
        };
        let file = CreatePopupMenu().unwrap_or_default();
        add(file, CMD_NEW, "&new draft\tctrl+n");
        add(file, CMD_OPEN, "&open…\tctrl+o");
        add(file, CMD_SAVE, "&save\tctrl+s");
        add(file, CMD_SAVE_AS, "save &as…\tctrl+shift+s");
        sep(file);
        let enc = CreatePopupMenu().unwrap_or_default();
        for (i, e) in ENCODINGS.iter().enumerate() {
            add(enc, CMD_ENC_BASE + i as u16, &format!("save as {}", e.name()));
        }
        sep(enc);
        add(enc, CMD_REOPEN_1251, "reopen as windows-1251");
        sub(file, enc, "&encoding");
        let eol = CreatePopupMenu().unwrap_or_default();
        for (i, e) in EOLS.iter().enumerate() {
            add(eol, CMD_EOL_BASE + i as u16, e.name());
        }
        sub(file, eol, "&line endings");
        sep(file);
        let drafts = CreatePopupMenu().unwrap_or_default();
        sub(file, drafts, "&drafts");
        sep(file);
        add(file, CMD_CLOSE_TAB, "&close tab\tctrl+w");
        add(file, CMD_REOPEN_CLOSED, "&reopen closed tab\tctrl+shift+t");
        sep(file);
        add(file, CMD_EXIT, "e&xit\talt+f4");
        sub(bar, file, "&file");

        let edit = CreatePopupMenu().unwrap_or_default();
        add(edit, CMD_UNDO, "&undo\tctrl+z");
        add(edit, CMD_REDO, "&redo\tctrl+y");
        sep(edit);
        add(edit, CMD_CUT, "cu&t\tctrl+x");
        add(edit, CMD_COPY, "&copy\tctrl+c");
        add(edit, CMD_PASTE, "&paste\tctrl+v");
        add(edit, CMD_SELECT_ALL, "select &all\tctrl+a");
        sep(edit);
        add(edit, CMD_FIND, "&find…\tctrl+f");
        add(edit, CMD_FIND_NEXT, "find &next\tf3");
        add(edit, CMD_FIND_PREV, "find pre&vious\tshift+f3");
        add(edit, CMD_REPLACE, "r&eplace…\tctrl+h");
        add(edit, CMD_GOTO, "&go to line…\tctrl+g");
        sub(bar, edit, "&edit");

        let view = CreatePopupMenu().unwrap_or_default();
        add(view, CMD_WRAP, "word &wrap\talt+z");
        add(view, CMD_PREVIEW, "markdown &preview\tctrl+shift+m");
        sub(bar, view, "&view");

        let tabs = CreatePopupMenu().unwrap_or_default();
        add(tabs, CMD_NEXT_TAB, "&next tab\tctrl+tab");
        add(tabs, CMD_PREV_TAB, "&previous tab\tctrl+shift+tab");
        sep(tabs);
        sub(bar, tabs, "&tabs");
        bar
    }
}

fn accelerators() -> HACCEL {
    let c = (FCONTROL | FVIRTKEY).0;
    let cs = (FCONTROL | FSHIFT | FVIRTKEY).0;
    let v = FVIRTKEY.0;
    let s = (FSHIFT | FVIRTKEY).0;
    let a = (FALT | FVIRTKEY).0;
    let k = |f: u8, key: u16, cmd: u16| ACCEL { fVirt: ACCEL_VIRT_FLAGS(f), key, cmd };
    let table = [
        k(c, b'N' as u16, CMD_NEW),
        k(c, b'O' as u16, CMD_OPEN),
        k(c, b'S' as u16, CMD_SAVE),
        k(cs, b'S' as u16, CMD_SAVE_AS),
        k(c, b'W' as u16, CMD_CLOSE_TAB),
        k(c, 0x73, CMD_CLOSE_TAB), // ctrl+f4
        k(cs, b'T' as u16, CMD_REOPEN_CLOSED),
        k(c, b'Z' as u16, CMD_UNDO),
        k(c, b'Y' as u16, CMD_REDO),
        k(cs, b'Z' as u16, CMD_REDO),
        k(c, b'X' as u16, CMD_CUT),
        k(c, b'C' as u16, CMD_COPY),
        k(c, b'V' as u16, CMD_PASTE),
        k(c, b'A' as u16, CMD_SELECT_ALL),
        k(c, b'F' as u16, CMD_FIND),
        k(v, 0x72, CMD_FIND_NEXT), // f3
        k(s, 0x72, CMD_FIND_PREV),
        k(c, b'H' as u16, CMD_REPLACE),
        k(c, b'G' as u16, CMD_GOTO),
        k(a, b'Z' as u16, CMD_WRAP),
        k(cs, b'M' as u16, CMD_PREVIEW),
        k(c, 0x09, CMD_NEXT_TAB), // ctrl+tab
        k(cs, 0x09, CMD_PREV_TAB),
        k(c, 0x22, CMD_NEXT_TAB), // ctrl+page down
        k(c, 0x21, CMD_PREV_TAB), // ctrl+page up
    ];
    // SAFETY: plain table creation; destroyed after the loop.
    unsafe { CreateAcceleratorTableW(&table).unwrap_or_default() }
}

fn invalidate(h: HWND) {
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(h), None, false);
    }
}

fn move_window(h: HWND, r: RECT) {
    // SAFETY: our own child window.
    unsafe {
        let _ = MoveWindow(h, r.left, r.top, r.right - r.left, r.bottom - r.top, true);
    }
}

/// moves a child only when its rect really changes (a rich edit control
/// re-lays out its whole text on every resize).
fn move_if_needed(h: HWND, r: RECT) {
    let mut now = RECT::default();
    // SAFETY: plain geometry queries on our own windows.
    unsafe {
        let _ = GetWindowRect(h, &mut now);
        if let Ok(parent) = GetParent(h) {
            let mut pts = [POINT { x: now.left, y: now.top }, POINT { x: now.right, y: now.bottom }];
            windows::Win32::Graphics::Gdi::MapWindowPoints(None, Some(parent), &mut pts);
            now = rect(pts[0].x, pts[0].y, pts[1].x, pts[1].y);
        }
    }
    if now != r {
        move_window(h, r);
    }
}

fn show(h: HWND, on: bool) {
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(h, if on { SW_SHOW } else { SW_HIDE });
    }
}

impl App {
    fn p(&self, v: i32) -> i32 {
        px(self.dpi, v)
    }

    fn bump(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn cur(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    // ---- tabs ----------------------------------------------------------

    fn new_tab(&mut self, doc: Doc) -> Option<usize> {
        let ed = match Editor::create(self.hwnd, 200) {
            Ok(e) => e,
            Err(e) => {
                dialogs::problem(self.hwnd, "ryad could not create a new tab", &e.message());
                return None;
            }
        };
        ed.apply_theme(&self.theme, &self.fonts.mono_face, self.fonts.editor_px);
        // born at the editor's place, hidden, so loading lays out only once
        move_window(ed.hwnd, self.editor_rc);
        self.tabs.push(Tab { doc, ed, pending_top: None });
        Some(self.tabs.len() - 1)
    }

    fn new_draft(&mut self) {
        let id = self.bump();
        let rev = self.bump();
        if let Some(i) = self.new_tab(Doc::new(id, rev)) {
            self.activate(i);
            self.touch();
        }
    }

    fn activate(&mut self, i: usize) {
        if self.tabs.is_empty() {
            self.new_draft();
            return;
        }
        let i = i.min(self.tabs.len() - 1);
        for (j, t) in self.tabs.iter().enumerate() {
            if j != i {
                show(t.ed.hwnd, false);
            }
        }
        self.active = i;
        self.layout();
        show(self.tabs[i].ed.hwnd, true);
        // SAFETY: focus to the text of the active tab.
        unsafe {
            let _ = SetFocus(Some(self.tabs[i].ed.hwnd));
        }
        if self.tabs[i].pending_top.is_some() {
            // after the focus and activation messages of start-up have run:
            // rich edit scrolls to the caret when it gets the focus
            let id = self.tabs[i].doc.id;
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_RESTORE_SCROLL, WPARAM(id as usize), LPARAM(0));
            }
        }
        self.update_title();
        self.view = (-1, -1, -1);
        self.on_view();
        invalidate(self.strip_hwnd);
    }

    fn update_title(&self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let title = format!("{}{} — {APP_NAME}", if t.doc.dirty() { "● " } else { "" }, t.doc.title());
        let w = wide(&title);
        // SAFETY: valid string.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(w.as_ptr()));
        }
    }

    fn tab_infos(&self) -> Vec<TabInfo> {
        self.tabs.iter().map(|t| TabInfo { title: t.doc.title(), dirty: t.doc.dirty() }).collect()
    }

    fn tab_tooltip(&self, i: usize) -> String {
        let Some(t) = self.tabs.get(i) else { return String::new() };
        let mut s = match &t.doc.path {
            Some(p) => p.display().to_string(),
            None => "draft, not saved to a file".to_string(),
        };
        if t.doc.dirty() {
            s.push_str(if t.doc.path.is_some() { "\nchanges not saved to the file yet" } else { "" });
        }
        s
    }

    /// closes tab `i` following the close rules. false when cancelled.
    fn close_tab(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() {
            return false;
        }
        let dirty = self.tabs[i].doc.dirty();
        if dirty {
            self.activate(i);
            let name = self.tabs[i].doc.title();
            let what = if self.tabs[i].doc.path.is_some() { "this file" } else { "this draft" };
            let choice = dialogs::ask(
                self.hwnd,
                Icon::None,
                &format!("{name} has unsaved changes"),
                &format!("save them to {what}, keep them as a draft for later (file › drafts), or discard them."),
                &[(1, "save"), (2, "keep as draft"), (3, "discard changes")],
                1,
                true,
            );
            match choice {
                1 => {
                    if !self.save(i, false) {
                        return false;
                    }
                }
                2 => return self.keep_as_draft(i),
                3 => {
                    let sure = dialogs::ask(
                        self.hwnd,
                        Icon::Warning,
                        &format!("discard the changes in {name}?"),
                        "they are deleted from ryad and cannot be brought back.",
                        &[(3, "discard")],
                        CANCEL,
                        true,
                    );
                    if sure != 3 {
                        return false;
                    }
                    let id = self.tabs[i].doc.id;
                    self.purge.push((id, 0));
                    let path = self.tabs[i].doc.path.clone();
                    let t = self.tabs.remove(i);
                    // a file tab may come back from disk, without the discarded changes
                    if let Some(p) = path {
                        let e = self.entry_of(&t, Kind::Closed, None);
                        self.closed.push(Closed::File(Entry { path: Some(p), ..e }));
                    }
                    drop(t);
                    self.after_close(i);
                    self.commit_now();
                    return true;
                }
                _ => return false,
            }
        }
        let t = self.tabs.remove(i);
        if t.doc.path.is_some() {
            let e = self.entry_of(&t, Kind::Closed, None);
            self.closed.push(Closed::File(e));
            if self.closed.len() > 20 {
                self.closed.remove(0);
            }
        }
        drop(t);
        self.after_close(i);
        self.touch();
        true
    }

    fn after_close(&mut self, i: usize) {
        if self.tabs.is_empty() {
            self.new_draft();
        } else {
            let next = if i >= self.tabs.len() { self.tabs.len() - 1 } else { i };
            self.activate(if self.active > i { self.active - 1 } else { next.min(self.tabs.len() - 1) });
        }
        invalidate(self.strip_hwnd);
    }

    /// "keep as draft": the tab closes only after the session has its text
    /// on disk. if that write fails, the tab stays open with its text.
    fn keep_as_draft(&mut self, i: usize) -> bool {
        let t = &self.tabs[i];
        let text = Arc::new(t.ed.text());
        let name = session::snapshot_name(t.doc.id, t.doc.rev);
        let snap = session::Snapshot { name: name.clone(), len: text.len() as u64, hash: files::fnv64(text.as_bytes()) };
        let e = self.entry_of(t, Kind::Kept, Some(snap));
        let id = t.doc.id;
        // out of the list (not destroyed) so the index has it only as kept
        let tab = self.tabs.remove(i);
        let old_active = self.active;
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.kept.push(e);
        self.commit_with(vec![(name, text)]);
        match self.writer.flush() {
            Ok(_) => {
                self.closed.push(Closed::Kept(id));
                drop(tab);
                self.after_close(i);
                true
            }
            Err(why) => {
                self.kept.pop();
                self.tabs.insert(i, tab);
                self.active = old_active;
                self.activate(i);
                dialogs::problem(self.hwnd, "the draft could not be kept", &format!("{why}\n\nthe tab stays open with its text."));
                false
            }
        }
    }

    fn reopen_closed(&mut self) {
        while let Some(c) = self.closed.pop() {
            match c {
                Closed::File(e) => {
                    if let Some(p) = &e.path {
                        if let Some(i) = self.find_open(p) {
                            self.activate(i);
                            return;
                        }
                        if let Some(i) = self.open_file(p, None, false) {
                            let t = &mut self.tabs[i];
                            if e.wrap {
                                t.ed.set_wrap(true);
                            }
                            let len = t.ed.len();
                            t.ed.set_sel(e.sel.0.min(len), e.sel.1.min(len));
                            t.pending_top = Some(e.top.min(len));
                            self.activate(i);
                            return;
                        }
                    }
                }
                Closed::Kept(id) => {
                    if let Some(k) = self.kept.iter().position(|e| e.id == id) {
                        self.open_kept(k);
                        return;
                    }
                }
            }
        }
    }

    fn open_kept(&mut self, k: usize) {
        let e = self.kept.remove(k);
        let mut e = e;
        e.kind = Kind::Tab;
        let tab = self.restore_tab(&e);
        if let Some(i) = tab {
            self.activate(i);
        }
        self.touch();
    }

    fn find_open(&self, p: &Path) -> Option<usize> {
        self.tabs.iter().position(|t| t.doc.path.as_deref().is_some_and(|q| files::same_path(p, q)))
    }

    // ---- opening ---------------------------------------------------------

    pub fn open_paths(&mut self, paths: &[PathBuf]) {
        let mut last = None;
        for p in paths {
            if let Some(i) = self.find_open(p) {
                last = Some(i);
                continue;
            }
            if let Some(i) = self.open_file(p, None, true) {
                last = Some(i);
            }
        }
        if let Some(i) = last {
            self.activate(i);
        }
        self.touch();
    }

    /// reads and decodes one file into a new tab (or the blank start draft).
    fn open_file(&mut self, p: &Path, forced: Option<Encoding>, ask: bool) -> Option<usize> {
        let t = Instant::now();
        // SAFETY: a wait cursor while a large file loads on this thread.
        let old = unsafe { SetCursor(LoadCursorW(None, IDC_WAIT).ok()) };
        let result = self.load_file(p, forced, ask);
        // SAFETY: restore the previous cursor.
        unsafe {
            SetCursor(Some(old));
        }
        trace!("open: {:.1} ms, {}", t.elapsed().as_secs_f64() * 1000.0, crate::memory_summary());
        result
    }

    fn load_file(&mut self, p: &Path, forced: Option<Encoding>, ask: bool) -> Option<usize> {
        let name = document::file_name(p);
        let (bytes, version) = match files::read(p) {
            Err(e) => {
                if ask {
                    dialogs::problem(self.hwnd, &format!("{name} could not be opened"), &format!("{}\n\n{e}", p.display()));
                }
                return None;
            }
            Ok(Err(e)) => {
                if ask {
                    dialogs::problem(self.hwnd, &format!("{name} was not opened"), &e.message());
                }
                return None;
            }
            Ok(Ok(x)) => x,
        };
        let decoded = match files::decode(&bytes, forced) {
            Ok(d) => d,
            Err(OpenError::NotUtf8) if ask => {
                let c = dialogs::ask(
                    self.hwnd,
                    Icon::None,
                    &format!("{name} is not utf-8"),
                    "it has no byte order mark and is not valid utf-8. it may be an older cyrillic text in windows-1251.",
                    &[(1, "open as windows-1251")],
                    1,
                    true,
                );
                if c != 1 {
                    return None;
                }
                match files::decode(&bytes, Some(Encoding::Win1251)) {
                    Ok(d) => d,
                    Err(e) => {
                        dialogs::problem(self.hwnd, &format!("{name} was not opened"), &e.message());
                        return None;
                    }
                }
            }
            Err(e) => {
                if ask {
                    dialogs::problem(self.hwnd, &format!("{name} was not opened"), &e.message());
                }
                return None;
            }
        };
        drop(bytes);
        // reuse an untouched start draft
        let reuse = self.tabs.get(self.active).is_some_and(|t| t.doc.is_blank(t.ed.len()));
        let id = self.bump();
        let rev = self.bump();
        let mut doc = Doc::new(id, rev);
        doc.path = Some(p.to_path_buf());
        doc.encoding = decoded.encoding;
        doc.eol = decoded.eol;
        doc.mixed = decoded.mixed;
        doc.disk = Some(version);
        doc.preview = document::is_markdown(p);
        let i = if reuse {
            let old = self.active;
            self.tabs[old].doc = doc;
            old
        } else {
            self.new_tab(doc)?
        };
        if Self::wants_wrap(&decoded.text) {
            self.tabs[i].ed.set_wrap(true);
        }
        self.loading = true;
        self.tabs[i].ed.load(&decoded.text);
        self.loading = false;
        // the control must hold exactly what was decoded (spike: it rewrites
        // some sequences); otherwise the file is not shown as editable text.
        if !self.verify_loaded(i, &decoded.text) {
            if reuse {
                self.tabs[i].doc = Doc::new(self.bump(), self.bump());
                self.loading = true;
                self.tabs[i].ed.load("");
                self.loading = false;
            } else {
                self.tabs.remove(i);
            }
            if ask {
                dialogs::problem(
                    self.hwnd,
                    &format!("{name} was not opened"),
                    "the windows text control changed some characters while loading it, so ryad left the file alone.",
                );
            }
            return None;
        }
        self.mark_saved(i, &decoded.text);
        self.tabs[i].doc.lines = count_lines(&decoded.text);
        self.tabs[i].ed.set_sel(0, 0);
        Some(i)
    }

    /// a file with very long lines opens with wrap on: unwrapped, rich edit
    /// needs seconds for one 10 mib line and nobody reads it sideways.
    fn wants_wrap(text: &str) -> bool {
        text.split(char::from(13)).any(|l| l.len() > LONG_LINE)
    }

    fn verify_loaded(&self, i: usize, text: &str) -> bool {
        let ed = &self.tabs[i].ed;
        let n = text.encode_utf16().count() as i32;
        ed.len() == n && (n > SAVEPOINT_LIMIT * 4 || ed.text() == text)
    }

    fn mark_saved(&mut self, i: usize, text: &str) {
        let d = &mut self.tabs[i].doc;
        d.saved_rev = d.rev;
        d.saved_len = text.encode_utf16().count() as i32;
        d.saved_hash = files::fnv64(text.as_bytes());
        d.snap_name = None;
    }

    // ---- saving ------------------------------------------------------------

    /// saves tab `i`. `as_new`: always ask for a path. false when the text
    /// did not reach a file (cancelled or failed); the tab stays as it was.
    fn save(&mut self, i: usize, as_new: bool) -> bool {
        // a clean tab whose file is as ryad left it: nothing to write (and a
        // file with mixed line endings stays byte for byte as it was)
        if !as_new
            && !self.tabs[i].doc.dirty()
            && let (Some(p), Some(known)) = (&self.tabs[i].doc.path, self.tabs[i].doc.disk)
            && matches!(files::check(p, known), DiskState::Same)
        {
            return true;
        }
        let target = match (&self.tabs[i].doc.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let doc = &self.tabs[i].doc;
                let suggested = match &doc.path {
                    Some(p) => document::file_name(p),
                    None if doc.draft_title.is_empty() => "draft.txt".to_string(),
                    None => format!("{}.txt", sanitize(&doc.draft_title)),
                };
                let folder = doc.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
                let Some(p) = dialogs::save_file(self.hwnd, &suggested, folder.as_deref()) else { return false };
                if let Some(j) = self.find_open(&p)
                    && j != i
                {
                    dialogs::problem(
                        self.hwnd,
                        &format!("{} is open in another tab", document::file_name(&p)),
                        "close that tab first, or choose another name.",
                    );
                    return false;
                }
                p
            }
        };
        let same_file = self.tabs[i].doc.path.as_deref().is_some_and(|p| files::same_path(p, &target));
        // a file that changed on disk since ryad read or wrote it
        if same_file
            && let Some(known) = self.tabs[i].doc.disk
            && let DiskState::Changed = files::check(&target, known)
        {
            let name = document::file_name(&target);
            let c = dialogs::ask(
                self.hwnd,
                Icon::Warning,
                &format!("{name} was changed by another program"),
                "saving now would overwrite those changes. you can save your text as a copy, reload the file from disk (your edits stay in undo), or overwrite it.",
                &[(1, "save a copy…"), (2, "reload from disk"), (3, "overwrite")],
                1,
                true,
            );
            match c {
                1 => return self.save(i, true),
                2 => {
                    self.reload(i);
                    return false;
                }
                3 => {}
                _ => return false,
            }
        }
        let text = self.tabs[i].ed.text();
        let (mut enc, eol) = (self.tabs[i].doc.encoding, self.tabs[i].doc.eol);
        let bytes = match files::encode(&text, enc, eol) {
            Ok(b) => b,
            Err(bad) => {
                let c = dialogs::ask(
                    self.hwnd,
                    Icon::Warning,
                    &format!("this text does not fit in {}", enc.name()),
                    &format!(
                        "{} character(s), the first one “{}”, cannot be written in {} without being lost. save it as utf-8 instead?",
                        bad.count,
                        bad.first,
                        enc.name()
                    ),
                    &[(1, "save as utf-8")],
                    1,
                    true,
                );
                if c != 1 {
                    return false;
                }
                enc = Encoding::Utf8;
                files::encode(&text, enc, eol).unwrap_or_default()
            }
        };
        let t = Instant::now();
        match files::save_atomic(&target, &bytes) {
            Ok(v) => {
                trace!("save: {} bytes in {:.1} ms", bytes.len(), t.elapsed().as_secs_f64() * 1000.0);
                let d = &mut self.tabs[i].doc;
                d.path = Some(target);
                d.disk = Some(v);
                d.encoding = enc;
                d.mixed = false;
                d.notice = None;
                self.mark_saved(i, &text);
                self.update_title();
                self.layout();
                invalidate(self.strip_hwnd);
                invalidate(self.status_hwnd);
                self.touch();
                true
            }
            Err(e) => {
                let name = document::file_name(&target);
                let c = dialogs::ask(
                    self.hwnd,
                    Icon::Warning,
                    &format!("{name} could not be saved"),
                    &format!("{}\n\nyour text is still here, unchanged. you can save it somewhere else.", e.message()),
                    &[(1, "save as…")],
                    1,
                    true,
                );
                if c == 1 { self.save(i, true) } else { false }
            }
        }
    }

    /// replaces the tab's text with the file on disk, as one undo step.
    fn reload(&mut self, i: usize) {
        let Some(p) = self.tabs[i].doc.path.clone() else { return };
        let read = files::read(&p);
        let Ok(Ok((bytes, v))) = read else {
            dialogs::problem(self.hwnd, "the file could not be read", &p.display().to_string());
            return;
        };
        match files::decode(&bytes, Some(self.tabs[i].doc.encoding).filter(|e| *e == Encoding::Win1251)) {
            Ok(d) => {
                self.tabs[i].ed.replace_all(&d.text);
                let doc = &mut self.tabs[i].doc;
                doc.lines = count_lines(&d.text);
                doc.disk = Some(v);
                doc.encoding = d.encoding;
                doc.eol = d.eol;
                doc.mixed = d.mixed;
                doc.notice = None;
                doc.rev = self.next_id + 1;
                self.next_id += 1;
                self.mark_saved(i, &d.text);
                self.tabs[i].ed.set_sel(0, 0);
                self.layout();
                self.update_title();
                invalidate(self.strip_hwnd);
                self.touch();
            }
            Err(e) => dialogs::problem(self.hwnd, "the file could not be reloaded", &e.message()),
        }
    }

    // ---- session -----------------------------------------------------------

    fn entry_of(&self, t: &Tab, kind: Kind, snapshot: Option<session::Snapshot>) -> Entry {
        let len = t.ed.len();
        let sel = t.ed.sel();
        Entry {
            kind,
            id: t.doc.id,
            path: t.doc.path.clone(),
            encoding: t.doc.encoding,
            eol: t.doc.eol,
            mixed: t.doc.mixed,
            disk: t.doc.disk,
            snapshot,
            wrap: t.ed.wrap(),
            preview: t.doc.preview,
            sel: (sel.0.min(len), sel.1.min(len)),
            top: t.ed.top(),
            title: t.doc.title(),
            when: crate::unix_time(),
        }
    }

    /// marks the session as changed: commit after COMMIT_IDLE of quiet, or
    /// right away when typing has gone on for COMMIT_MAX.
    fn touch(&mut self) {
        let now = Instant::now();
        let first = *self.pending.get_or_insert(now);
        if now.duration_since(first) >= COMMIT_MAX {
            self.commit_now();
        } else {
            // SAFETY: our own window's timer.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_COMMIT, COMMIT_IDLE, None);
            }
        }
    }

    fn commit_now(&mut self) {
        self.commit_with(Vec::new());
    }

    /// builds the next index from the current state and hands it to the
    /// writer, with copies of every changed text not committed yet.
    fn commit_with(&mut self, mut snapshots: Vec<(String, Arc<String>)>) {
        // SAFETY: our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_COMMIT);
        }
        self.pending = None;
        let t0 = Instant::now();
        let mut entries = Vec::new();
        for i in 0..self.tabs.len() {
            let snap = if self.tabs[i].doc.dirty() {
                if self.tabs[i].doc.needs_snapshot() || self.tabs[i].doc.snap_name.is_none() {
                    let text = Arc::new(self.tabs[i].ed.text());
                    let d = &mut self.tabs[i].doc;
                    let name = session::snapshot_name(d.id, d.rev);
                    d.snap_rev = d.rev;
                    d.snap_name = Some(name.clone());
                    snapshots.push((name, text));
                }
                let d = &self.tabs[i].doc;
                let name = d.snap_name.clone().unwrap_or_default();
                let text = snapshots.iter().find(|(n, _)| *n == name).map(|(_, t)| t.clone());
                Some(match text {
                    Some(t) => session::Snapshot { name, len: t.len() as u64, hash: files::fnv64(t.as_bytes()) },
                    None => self.committed_snapshot(d.id, &name),
                })
            } else {
                None
            };
            entries.push(self.entry_of(&self.tabs[i], Kind::Tab, snap));
        }
        entries.extend(self.kept.iter().cloned());
        for c in &self.closed {
            if let Closed::File(e) = c {
                entries.push(e.clone());
            }
        }
        self.generation += 1;
        let index = Index { generation: self.generation, next_id: self.next_id + 1, active: self.active, entries };
        self.remember_snapshots(&index, &snapshots);
        for p in &mut self.purge {
            if p.1 == 0 {
                p.1 = self.generation;
            }
        }
        let purge: Vec<u64> = self.purge.iter().map(|p| p.0).collect();
        trace!(
            "session: generation {} queued, {} snapshot(s), {:.1} ms on the ui thread",
            self.generation,
            snapshots.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        self.writer.send(Job::Commit(session::Commit { generation: self.generation, index, snapshots, purge }));
    }

    /// snapshot metadata of copies the session already holds, by name.
    fn committed_snapshot(&self, _id: u64, name: &str) -> session::Snapshot {
        self.known_snaps.iter().find(|s| s.name == name).cloned().unwrap_or(session::Snapshot { name: name.to_string(), len: 0, hash: 0 })
    }

    fn remember_snapshots(&mut self, index: &Index, fresh: &[(String, Arc<String>)]) {
        let _ = fresh;
        self.known_snaps = index.entries.iter().filter_map(|e| e.snapshot.clone()).collect();
    }

    fn on_session_done(&mut self) {
        let error = self.writer.status.lock().map(|s| s.error.clone()).unwrap_or(None);
        let had = self.session_error.is_some();
        match error {
            Some(e) => {
                // everything must be written again: the failed commit may
                // have left any of its snapshots unwritten
                for t in &mut self.tabs {
                    t.doc.snap_rev = 0;
                    t.doc.snap_name = None;
                }
                self.session_error = Some(e);
                // SAFETY: our own timer: try again in a while.
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_RETRY, 10_000, None);
                }
            }
            None => {
                self.session_error = None;
                let done = self.writer.status.lock().map(|s| s.done).unwrap_or(0);
                self.purge.retain(|p| p.1 > done);
            }
        }
        if had != self.session_error.is_some() {
            self.layout();
        }
    }

    fn restore(&mut self) {
        let t = Instant::now();
        let restored = session::restore(&self.session_dir);
        let Some(r) = restored else {
            self.new_draft();
            trace!("restore: no session, {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            return;
        };
        self.restore_problems.extend(r.problems);
        self.next_id = r.index.next_id.max(self.next_id);
        self.generation = r.index.generation;
        self.known_snaps = r.index.entries.iter().filter_map(|e| e.snapshot.clone()).collect();
        for e in &r.index.entries {
            match e.kind {
                Kind::Tab => {
                    let _ = self.restore_tab(e);
                }
                Kind::Kept => self.kept.push(e.clone()),
                Kind::Closed => self.closed.push(Closed::File(e.clone())),
            }
        }
        if self.tabs.is_empty() {
            self.new_draft();
        }
        self.active = r.index.active.min(self.tabs.len() - 1);
        trace!("restore: {} tab(s), {} kept, from backup {}, {:.1} ms", self.tabs.len(), self.kept.len(), r.from_backup, t.elapsed().as_secs_f64() * 1000.0);
    }

    /// one entry of the session as a tab. a problem with it (a missing file,
    /// a damaged copy) is shown in that tab; the others load normally.
    fn restore_tab(&mut self, e: &Entry) -> Option<usize> {
        let rev = self.bump();
        let mut doc = Doc::new(e.id, rev);
        doc.path = e.path.clone();
        doc.encoding = e.encoding;
        doc.eol = e.eol;
        doc.mixed = e.mixed;
        doc.disk = e.disk;
        doc.preview = e.preview;
        doc.draft_title = if e.path.is_none() { e.title.clone() } else { String::new() };
        if doc.draft_title == document::NEW_DRAFT {
            doc.draft_title.clear();
        }
        let i = self.new_tab(doc)?;
        let mut text = String::new();
        let mut clean = true;
        if let Some(s) = &e.snapshot {
            match session::read_snapshot(&self.session_dir, s) {
                Ok(t) => {
                    text = t;
                    clean = false;
                    let d = &mut self.tabs[i].doc;
                    d.saved_rev = 0;
                    d.snap_rev = rev;
                    d.snap_name = Some(s.name.clone());
                }
                Err(why) => {
                    let what = e.path.as_ref().map(|p| document::file_name(p)).unwrap_or_else(|| e.title.clone());
                    self.restore_problems.push(format!("{what}: {why}."));
                    self.tabs[i].doc.notice = Some(Notice::RestoreFailed(why));
                }
            }
        }
        if let Some(p) = &e.path {
            let on_disk = match files::read(p) {
                Ok(Ok((bytes, v))) => files::decode(&bytes, Some(e.encoding).filter(|x| *x == Encoding::Win1251)).ok().map(|d| (d, v)),
                _ => None,
            };
            match (clean, on_disk) {
                (true, Some((d, v))) => {
                    // a clean tab reads its file again
                    text = d.text;
                    let doc = &mut self.tabs[i].doc;
                    doc.disk = Some(v);
                    doc.eol = d.eol;
                    doc.mixed = d.mixed;
                    doc.encoding = d.encoding;
                }
                (true, None) => {
                    if self.tabs[i].doc.notice.is_none() {
                        self.tabs[i].doc.notice = Some(Notice::Missing);
                    }
                }
                (false, None) => {
                    if !p.exists() {
                        self.tabs[i].doc.notice = Some(Notice::Missing);
                    }
                }
                (false, Some((d, v))) => {
                    if Some(v) != e.disk {
                        if d.text == text {
                            // ctrl+s finished but the session did not hear of
                            // it: the file already has these edits
                            clean = true;
                            self.tabs[i].doc.disk = Some(v);
                        } else {
                            self.tabs[i].doc.notice = Some(Notice::ChangedOnDisk);
                        }
                    }
                }
            }
        }
        // wrap before loading: unwrapped, very long lines lay out slowly
        if e.wrap {
            self.tabs[i].ed.set_wrap(true);
        }
        self.loading = true;
        self.tabs[i].ed.load(&text);
        self.loading = false;
        self.tabs[i].doc.lines = count_lines(&text);
        if clean {
            self.mark_saved(i, &text);
        }
        let t = &mut self.tabs[i];
        let len = t.ed.len();
        t.ed.set_sel(e.sel.0.clamp(0, len), e.sel.1.clamp(0, len));
        t.pending_top = Some(e.top.clamp(0, len));
        Some(i)
    }

    // ---- layout and painting ---------------------------------------------

    fn layout(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: our own window.
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let (w, h) = (rc.right, rc.bottom);
        let bar_h = crate::menubar::height(self.dpi);
        let strip_h = tabs::height(self.dpi);
        let status_h = self.p(26);
        let banner = self.banner_content();
        let banner_h = if banner.is_some() { self.p(36) } else { 0 };
        let find_h = self.find.height(self.dpi);
        move_window(self.menubar_hwnd, rect(0, 0, w, bar_h));
        move_window(self.strip_hwnd, rect(0, bar_h, w, bar_h + strip_h));
        move_window(self.status_hwnd, rect(0, h - status_h, w, h));
        show(self.banner_hwnd, banner.is_some());
        move_window(self.banner_hwnd, rect(0, bar_h + strip_h, w, bar_h + strip_h + banner_h));
        if let Some((_, buttons)) = &banner {
            self.place_banner_buttons(w, banner_h, buttons);
        }
        let top = bar_h + strip_h + banner_h;
        let bottom = h - status_h - find_h;
        // SAFETY: screen dc for text measurement only.
        unsafe {
            let dc = GetDC(Some(self.hwnd));
            self.find.layout(self.dpi, 0, bottom, w, dc, &self.fonts);
            let infos = self.tab_infos();
            self.strip.layout(dc, &self.fonts, self.dpi, w, &infos, self.active);
            self.menubar.layout(dc, &self.fonts, self.dpi);
            ReleaseDC(Some(self.hwnd), dc);
        }
        show(self.find.hwnd, self.find.open);
        let gutter_w = self.gutter_width();
        move_window(self.gutter_hwnd, rect(0, top, gutter_w, bottom));
        self.gutter_w = gutter_w;
        let with_preview = self.tabs.get(self.active).is_some_and(|t| t.doc.preview);
        let mut right = w;
        if with_preview {
            let sw = self.p(6);
            let min = self.p(160);
            let x = gutter_w + ((w - gutter_w) as f32 * self.split) as i32;
            let x = x.clamp(gutter_w + min, (w - min - sw).max(gutter_w + min));
            right = x;
            move_window(self.split_hwnd, rect(x, top, x + sw, bottom));
            move_if_needed(self.preview.hwnd, rect(x + sw, top, w, bottom));
        }
        show(self.split_hwnd, with_preview);
        show(self.preview.hwnd, with_preview);
        self.editor_rc = rect(gutter_w, top, right, bottom);
        if let Some(t) = self.tabs.get(self.active) {
            move_if_needed(t.ed.hwnd, self.editor_rc);
        }
        if with_preview && self.preview_stale() {
            // SAFETY: our own timer; draws the preview once things settle.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_PREVIEW, 30, None);
            }
        }
        // the tooltip tool covers the whole strip
        let mut ti = TTTOOLINFOW { cbSize: size_of::<TTTOOLINFOW>() as u32, hwnd: self.strip_hwnd, uId: 1, rect: rect(0, 0, w, strip_h), ..Default::default() };
        // SAFETY: our own tooltip.
        unsafe {
            SendMessageW(self.tooltip, TTM_NEWTOOLRECTW, None, Some(LPARAM(&mut ti as *mut _ as isize)));
        }
        invalidate(self.strip_hwnd);
        invalidate(self.status_hwnd);
        invalidate(self.gutter_hwnd);
        invalidate(self.banner_hwnd);
    }

    /// colours and size of the preview's current look, for the stale check.
    fn preview_width(&self) -> i32 {
        let mut r = RECT::default();
        // SAFETY: plain query on our own control.
        unsafe {
            let _ = GetClientRect(self.preview.hwnd, &mut r);
        }
        (r.right - 2 * self.p(24) - self.p(4)).max(self.p(80))
    }

    fn preview_stamp(&self) -> u64 {
        ((ui::colorref(self.theme.ground).0 as u64) << 32) | ui::colorref(self.accent).0 as u64 ^ (self.dpi as u64) << 24
    }

    fn preview_stale(&self) -> bool {
        let Some(t) = self.tabs.get(self.active) else { return false };
        self.preview_key != (t.doc.id, t.doc.rev, self.preview_width(), self.preview_stamp())
    }

    /// draws the active tab's markdown into the preview, if it changed.
    fn render_preview(&mut self) {
        // SAFETY: our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PREVIEW);
        }
        let Some(t) = self.tabs.get(self.active) else { return };
        if !t.doc.preview || !self.preview_stale() {
            return;
        }
        let t0 = Instant::now();
        let style = crate::markdown::Style {
            ink: self.theme.ink,
            ink2: self.theme.ink2,
            accent: self.accent,
            code_bg: if self.theme.dark { ui::rgb(0x2a2a2d) } else { ui::rgb(0xececee) },
            hair: self.theme.hair,
            body_face: self.fonts.ui_name.clone(),
            head_face: self.fonts.display_name.clone(),
            mono_face: self.fonts.mono_name.clone(),
            body_px: self.p(15),
            width_px: self.preview_width(),
            dpi_scale: 1440.0 / ui::system_dpi() as f32,
        };
        let units = t.ed.len() as usize;
        let blocks = if units > crate::preview::MAX_UNITS {
            vec![crate::markdown::Block::Para(vec![crate::markdown::Inline::Text(format!(
                "the preview is off for texts over {} mb, to keep typing quick.",
                crate::preview::MAX_UNITS / 1024 / 1024
            ))])]
        } else {
            crate::markdown::parse(&t.ed.text())
        };
        let rtf = crate::markdown::to_rtf(&blocks, &style);
        self.preview.set_rtf(&rtf, units);
        self.preview.scroll_to_fraction(crate::preview::fraction_of(t.ed.hwnd));
        self.preview_key = (t.doc.id, t.doc.rev, self.preview_width(), self.preview_stamp());
        trace!("preview: {} units, {} bytes of rtf, {:.1} ms", units, rtf.len(), t0.elapsed().as_secs_f64() * 1000.0);
    }

    fn gutter_width(&mut self) -> i32 {
        // logical lines of the active tab. not EM_GETLINECOUNT: that makes
        // rich edit lay out the whole text again (~1 s for 10 mib, measured).
        let lines = self.tabs.get(self.active).map(|t| t.doc.lines);
        let digits = lines.unwrap_or(1).max(1).to_string().len().max(3) as i32;
        // SAFETY: screen dc for measurement only.
        let digit_w = unsafe {
            let dc = GetDC(Some(self.hwnd));
            let w = ui::text_width(dc, "0", self.fonts.mono);
            ReleaseDC(Some(self.hwnd), dc);
            w
        };
        digit_w * digits + self.p(24)
    }

    /// called after anything that may change what the gutter or status show.
    fn on_view(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        // SAFETY: plain queries.
        let first = unsafe { SendMessageW(t.ed.hwnd, windows::Win32::UI::Controls::EM_GETFIRSTVISIBLELINE, None, None).0 } as i32;
        let sel = t.ed.sel();
        let key = (first, sel.0, sel.1);
        if key != self.view {
            if key.0 != self.view.0 {
                invalidate(self.gutter_hwnd);
                if t.doc.preview {
                    self.preview.scroll_to_fraction(crate::preview::fraction_of(t.ed.hwnd));
                }
            }
            self.view = key;
            self.update_status();
        }
    }

    /// 0-based logical line of `cp` in the active tab, using the cache when
    /// the text is unchanged and the distance is short.
    fn logical_line(&mut self, cp: i32) -> i32 {
        let Some(t) = self.tabs.get_mut(self.active) else { return 0 };
        if !t.ed.wrap() {
            return t.ed.line_of(cp);
        }
        let (id, rev, ccp, cline) = self.line_cache;
        let line = if id == t.doc.id && rev == t.doc.rev && ccp >= 0 && (cp - ccp).abs() < 200_000 {
            let (a, b) = if cp >= ccp { (ccp, cp) } else { (cp, ccp) };
            let breaks = t.ed.range(a, b).matches('\r').count() as i32;
            if cp >= ccp { cline + breaks } else { cline - breaks }
        } else {
            t.ed.line_of(cp)
        };
        self.line_cache = (t.doc.id, t.doc.rev, cp, line);
        line
    }

    fn update_status(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let (a, b) = t.ed.sel();
        let caret = b;
        // column in characters (code points) from the line start
        let back = t.ed.range((caret - 100_000).max(0), caret);
        let col = match back.rfind('\r') {
            Some(k) => back[k + 1..].chars().count(),
            None => back.chars().count(),
        } + 1;
        let enc = t.doc.encoding.name().to_string();
        let eol = if t.doc.mixed { format!("mixed → {}", t.doc.eol.name()) } else { t.doc.eol.name().to_string() };
        let wrap = if t.ed.wrap() { "wrap" } else { "no wrap" }.to_string();
        let selected = if a != b { format!(" · {} selected", t.ed.range(a.min(b), a.max(b)).chars().count()) } else { String::new() };
        let line = self.logical_line(caret) + 1;
        self.status_text = [format!("ln {line}, col {col}{selected}"), enc, eol, wrap];
        invalidate(self.status_hwnd);
    }

    fn paint_status(&self, hwnd: HWND) {
        let p = Paint::begin(hwnd);
        p.fill(p.rect, self.theme.ground);
        p.fill(rect(0, 0, p.rect.right, 1), self.theme.hair);
        let pad = self.p(12);
        p.text(&self.status_text[0], rect(pad, 0, p.rect.right / 2, p.rect.bottom), self.fonts.small, self.theme.ink2, Align::Left);
        let mut x = p.rect.right - pad;
        for s in self.status_text[1..].iter().rev() {
            let w = ui::text_width(p.dc, s, self.fonts.small);
            p.text(s, rect(x - w, 0, x, p.rect.bottom), self.fonts.small, self.theme.ink2, Align::Right);
            x -= w + self.p(20);
        }
    }

    fn paint_gutter(&mut self, hwnd: HWND) {
        let theme = self.theme;
        let p = Paint::begin(hwnd);
        p.fill(p.rect, theme.raised);
        let height = p.rect.bottom;
        let lines = match self.tabs.get(self.active) {
            Some(t) => t.ed.visible_lines(height),
            None => return,
        };
        let Some(&(_, first_cp)) = lines.first() else { return };
        let mut n = self.logical_line(first_cp);
        let t = &self.tabs[self.active];
        let line_h = lines.get(1).map(|l| l.0 - lines[0].0).filter(|h| *h > 0).unwrap_or(self.p(18));
        let mut last_y = i32::MIN;
        for (k, &(y, cp)) in lines.iter().enumerate() {
            let starts = t.ed.starts_line(cp);
            if k > 0 && starts {
                n += 1;
            }
            // the empty line after a final line break: rich edit reports the
            // end of the previous line for it
            let y = if y <= last_y { last_y + line_h } else { y };
            last_y = y;
            if starts {
                let r = rect(0, y, p.rect.right - self.p(12), y + line_h);
                p.text(&(n + 1).to_string(), r, self.fonts.mono, theme.ink3, Align::Right);
            }
        }
    }

    fn banner_content(&self) -> Option<(String, Vec<(u16, &'static str)>)> {
        if let Some(e) = &self.session_error {
            return Some((
                format!("ryad could not keep your unsaved text: {e}. it tries again every 10 seconds."),
                vec![(CMD_BANNER_A, "retry"), (CMD_BANNER_B, "save as…")],
            ));
        }
        let t = self.tabs.get(self.active)?;
        match &t.doc.notice {
            None => None,
            Some(Notice::Missing) => {
                let path = t.doc.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                Some((
                    format!("{path} is not on disk anymore. the text here is what ryad had."),
                    vec![(CMD_BANNER_A, "save as…"), (CMD_BANNER_B, "open another…")],
                ))
            }
            Some(Notice::ChangedOnDisk) => Some((
                "the file changed on disk after these edits. saving will ask what to do.".into(),
                vec![(CMD_BANNER_A, "reload from disk"), (CMD_BANNER_B, "save as…")],
            )),
            Some(Notice::RestoreFailed(why)) => {
                Some((format!("this tab could not be fully restored: {why}."), vec![(CMD_BANNER_A, "close tab"), (CMD_BANNER_B, "dismiss")]))
            }
        }
    }

    fn place_banner_buttons(&self, w: i32, h: i32, buttons: &[(u16, &'static str)]) {
        // SAFETY: screen dc for measurement only.
        let dc = unsafe { GetDC(Some(self.hwnd)) };
        let mut x = w - self.p(12);
        for (k, b) in self.banner_buttons.iter().enumerate().rev() {
            match buttons.get(k) {
                Some((_, label)) => {
                    let bw = ui::text_width(dc, label, self.fonts.small) + self.p(24);
                    find::set_text(*b, label);
                    move_window(*b, rect(x - bw, (h - self.p(26)) / 2, x, (h + self.p(26)) / 2));
                    show(*b, true);
                    x -= bw + self.p(6);
                }
                None => show(*b, false),
            }
        }
        // SAFETY: release the dc taken above.
        unsafe {
            ReleaseDC(Some(self.hwnd), dc);
        }
    }

    fn set_banner_fonts(&self) {
        for b in self.banner_buttons {
            // SAFETY: our own buttons.
            unsafe {
                SendMessageW(b, WM_SETFONT, Some(WPARAM(self.fonts.small.0 as usize)), Some(LPARAM(1)));
            }
        }
    }

    fn banner_command(&mut self, which: u16) {
        if self.session_error.is_some() {
            if which == CMD_BANNER_A {
                self.commit_now();
            } else {
                self.save(self.active, true);
            }
            return;
        }
        let i = self.active;
        let Some(notice) = self.tabs.get(i).and_then(|t| t.doc.notice.clone()) else { return };
        match (notice, which) {
            (Notice::Missing, CMD_BANNER_A) | (Notice::ChangedOnDisk, CMD_BANNER_B) => {
                self.save(i, true);
            }
            (Notice::Missing, _) => self.command(CMD_OPEN),
            (Notice::ChangedOnDisk, _) => {
                let c = dialogs::ask(
                    self.hwnd,
                    Icon::Warning,
                    "reload the file from disk?",
                    "the text in this tab is replaced by the file. ctrl+z brings your edits back.",
                    &[(1, "reload")],
                    1,
                    true,
                );
                if c == 1 {
                    self.reload(i);
                }
            }
            (Notice::RestoreFailed(_), CMD_BANNER_A) => {
                self.close_tab(i);
            }
            (Notice::RestoreFailed(_), _) => {
                self.tabs[i].doc.notice = None;
                self.layout();
            }
        }
    }

    // ---- find --------------------------------------------------------------

    fn open_find(&mut self, mode: Mode) {
        let was = self.find.open;
        self.find.open = true;
        self.find.set_mode(mode);
        if mode != Mode::GoTo {
            // a short single-line selection becomes the search text
            if let Some(t) = self.tabs.get(self.active) {
                let (a, b) = t.ed.sel();
                let s = t.ed.range(a.min(b), a.max(b));
                if !s.is_empty() && s.len() < 200 && !s.contains('\r') {
                    find::set_text(self.find.find, &s);
                }
            }
        } else {
            find::set_text(self.find.find, "");
            let count = self.tabs.get_mut(self.active).map(|t| t.ed.line_count()).unwrap_or(1);
            self.find.line_hint = format!("1–{count}");
        }
        let _ = was;
        self.layout();
        // SAFETY: focus into the panel's field, text selected.
        unsafe {
            let _ = SetFocus(Some(self.find.find));
            SendMessageW(self.find.find, windows::Win32::UI::Controls::EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        invalidate(self.find.hwnd);
    }

    fn close_find(&mut self) {
        if !self.find.open {
            return;
        }
        self.find.open = false;
        self.layout();
        self.focus_editor();
    }

    fn focus_editor(&self) {
        if let Some(t) = self.tabs.get(self.active) {
            // SAFETY: focus back to the text.
            unsafe {
                let _ = SetFocus(Some(t.ed.hwnd));
            }
        }
    }

    fn set_find_status(&mut self, s: &str) {
        self.find.status = s.to_string();
        invalidate(self.find.hwnd);
    }

    fn find_next(&mut self, down: bool) {
        let what = find::text_of(self.find.find);
        if what.is_empty() {
            if !self.find.open {
                self.open_find(Mode::Find);
            }
            self.set_find_status("type something to find");
            return;
        }
        let case = self.find.case;
        let Some(t) = self.tabs.get(self.active) else { return };
        let (a, b) = t.ed.sel();
        let from = if down { a.max(b) } else { a.min(b) };
        let mut status = "";
        let found = t.ed.find(&what, from, down, case).or_else(|| {
            let r = t.ed.find(&what, if down { 0 } else { t.ed.len() }, down, case);
            if r.is_some() {
                status = if down { "from the top" } else { "from the end" };
            }
            r
        });
        match found {
            Some((x, y)) => {
                t.ed.show_range(x, y);
                self.set_find_status(status);
            }
            None => self.set_find_status("no matches"),
        }
        self.on_view();
    }

    fn replace_one(&mut self) {
        let what = find::text_of(self.find.find);
        let with = find::text_of(self.find.replace);
        if what.is_empty() {
            self.set_find_status("type something to find");
            return;
        }
        let case = self.find.case;
        if let Some(t) = self.tabs.get(self.active) {
            let (a, b) = t.ed.sel();
            let sel = t.ed.range(a.min(b), a.max(b));
            let matches = if case { sel == what } else { sel.to_lowercase() == what.to_lowercase() };
            if matches && a != b {
                t.ed.replace_sel(&with);
            }
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        let what = find::text_of(self.find.find);
        let with = find::text_of(self.find.replace);
        if what.is_empty() {
            self.set_find_status("type something to find");
            return;
        }
        let case = self.find.case;
        let Some(t) = self.tabs.get(self.active) else { return };
        let matches = t.ed.find_all(&what, case);
        if matches.is_empty() {
            self.set_find_status("no matches");
            return;
        }
        let units: Vec<u16> = t.ed.text().encode_utf16().collect();
        let with_u: Vec<u16> = with.encode_utf16().collect();
        let mut out = Vec::with_capacity(units.len());
        let mut at = 0usize;
        for &(a, b) in &matches {
            out.extend_from_slice(&units[at..a as usize]);
            out.extend_from_slice(&with_u);
            at = b as usize;
        }
        out.extend_from_slice(&units[at..]);
        let caret = t.ed.sel().0;
        t.ed.replace_all(&String::from_utf16_lossy(&out));
        let len = t.ed.len();
        t.ed.set_sel(caret.min(len), caret.min(len));
        let n = matches.len();
        self.set_find_status(&format!("replaced {n} · ctrl+z undoes all"));
        self.on_view();
    }

    fn go_to_line(&mut self) {
        let text = find::text_of(self.find.find);
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let count = t.ed.line_count();
        match text.trim().parse::<i32>() {
            Ok(n) if n >= 1 => {
                let n = n.min(count);
                let cp = t.ed.line_start(n - 1);
                t.ed.show_range(cp, cp);
                self.close_find();
                self.on_view();
            }
            _ => self.set_find_status(&format!("a line from 1 to {count}")),
        }
    }

    // ---- commands ------------------------------------------------------

    /// alt alone, f10 and alt+letter open ryad's own menus (there is no
    /// win32 menu bar to do it). true when the key was used.
    fn menu_key(&mut self, msg: &MSG) -> bool {
        const VK_MENU: usize = 0x12;
        const VK_F10: usize = 0x79;
        match msg.message {
            WM_SYSKEYDOWN if msg.wParam.0 == VK_MENU => {
                // key repeat while held keeps the flag; anything else clears it
                if (msg.lParam.0 >> 30) & 1 == 0 {
                    self.menubar.alt_alone = true;
                }
                false
            }
            WM_SYSKEYDOWN if msg.wParam.0 == VK_F10 => {
                self.menubar.alt_alone = false;
                let (bar, owner) = (self.menubar_hwnd, self.hwnd);
                self.menubar.track(bar, owner, 0, true);
                true
            }
            WM_SYSKEYDOWN => {
                self.menubar.alt_alone = false;
                let letter = char::from_u32(msg.wParam.0 as u32).filter(|c| c.is_ascii_alphabetic());
                match letter.and_then(|c| self.menubar.by_letter(c)) {
                    Some(i) => {
                        let (bar, owner) = (self.menubar_hwnd, self.hwnd);
                        self.menubar.track(bar, owner, i, true);
                        true
                    }
                    None => false,
                }
            }
            WM_SYSKEYUP if msg.wParam.0 == VK_MENU && self.menubar.alt_alone => {
                self.menubar.alt_alone = false;
                let (bar, owner) = (self.menubar_hwnd, self.hwnd);
                self.menubar.track(bar, owner, 0, true);
                true
            }
            WM_KEYDOWN | WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                self.menubar.alt_alone = false;
                false
            }
            _ => false,
        }
    }

    fn focused_field(&self) -> Option<HWND> {
        // SAFETY: plain query.
        let f = unsafe { GetFocus() };
        (f == self.find.find || f == self.find.replace || f == self.preview.hwnd).then_some(f)
    }

    fn command(&mut self, id: u16) {
        match id {
            CMD_NEW => self.new_draft(),
            CMD_OPEN => {
                let start = self.tabs.get(self.active).and_then(|t| t.doc.path.as_ref()).and_then(|p| p.parent().map(Path::to_path_buf));
                let paths = dialogs::open_files(self.hwnd, start.as_deref());
                if !paths.is_empty() {
                    self.open_paths(&paths);
                }
            }
            CMD_SAVE => {
                self.save(self.active, false);
            }
            CMD_SAVE_AS => {
                self.save(self.active, true);
            }
            CMD_CLOSE_TAB => {
                self.close_tab(self.active);
            }
            CMD_REOPEN_CLOSED => self.reopen_closed(),
            CMD_EXIT => {
                // SAFETY: the same path as the close button.
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            CMD_REOPEN_1251 => self.reopen_as_1251(),
            CMD_DELETE_DRAFTS => self.delete_kept_drafts(),
            CMD_UNDO | CMD_REDO | CMD_CUT | CMD_COPY | CMD_PASTE | CMD_SELECT_ALL => self.edit_command(id),
            CMD_FIND => self.open_find(Mode::Find),
            CMD_REPLACE => self.open_find(Mode::Replace),
            CMD_GOTO => self.open_find(Mode::GoTo),
            CMD_FIND_NEXT => self.find_next(true),
            CMD_FIND_PREV => self.find_next(false),
            CMD_WRAP => {
                if let Some(t) = self.cur() {
                    let on = !t.ed.wrap();
                    t.ed.set_wrap(on);
                }
                self.line_cache = (0, 0, -1, 0);
                self.layout();
                self.update_status();
                self.touch();
            }
            CMD_NEXT_TAB if !self.tabs.is_empty() => self.activate((self.active + 1) % self.tabs.len()),
            CMD_PREV_TAB if !self.tabs.is_empty() => self.activate((self.active + self.tabs.len() - 1) % self.tabs.len()),
            CMD_TAB_LIST => self.tab_list_menu(),
            CMD_PREVIEW => {
                if let Some(t) = self.cur() {
                    t.doc.preview = !t.doc.preview;
                }
                self.preview_key = (0, 0, 0, 0);
                self.layout();
                self.render_preview();
                self.focus_editor();
                self.touch();
            }
            CMD_BANNER_A | CMD_BANNER_B => self.banner_command(id),
            x if (CMD_ENC_BASE..CMD_ENC_BASE + ENCODINGS.len() as u16).contains(&x) => {
                let e = ENCODINGS[(x - CMD_ENC_BASE) as usize];
                self.set_format(Some(e), None);
            }
            x if (CMD_EOL_BASE..CMD_EOL_BASE + EOLS.len() as u16).contains(&x) => {
                let e = EOLS[(x - CMD_EOL_BASE) as usize];
                self.set_format(None, Some(e));
            }
            x if (CMD_TAB_BASE..CMD_TAB_BASE + 1000).contains(&x) => self.activate((x - CMD_TAB_BASE) as usize),
            x if (CMD_DRAFT_BASE..CMD_DRAFT_BASE + 1000).contains(&x) => {
                let k = (x - CMD_DRAFT_BASE) as usize;
                if k < self.kept.len() {
                    self.open_kept(k);
                }
            }
            _ => {}
        }
    }

    fn edit_command(&mut self, id: u16) {
        use windows::Win32::UI::Controls::{EM_SETSEL, EM_UNDO};
        let field = self.focused_field();
        let target = field.or_else(|| self.tabs.get(self.active).map(|t| t.ed.hwnd));
        let Some(h) = target else { return };
        let msg = match id {
            CMD_UNDO => EM_UNDO,
            CMD_REDO if field.is_some() => EM_UNDO, // the edit field has one level
            CMD_REDO => {
                if let Some(t) = self.tabs.get(self.active) {
                    t.ed.redo();
                }
                return;
            }
            CMD_CUT => WM_CUT,
            CMD_COPY => WM_COPY,
            CMD_PASTE => WM_PASTE,
            CMD_SELECT_ALL => {
                // SAFETY: select everything in the focused control.
                unsafe {
                    if field.is_some() {
                        SendMessageW(h, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                    } else if let Some(t) = self.tabs.get(self.active) {
                        t.ed.set_sel(0, -1);
                    }
                }
                return;
            }
            _ => return,
        };
        // SAFETY: standard clipboard/undo messages to our own control.
        unsafe {
            SendMessageW(h, msg, None, None);
        }
    }

    fn set_format(&mut self, enc: Option<Encoding>, eol: Option<Eol>) {
        let rev = self.bump();
        let Some(t) = self.cur() else { return };
        let mut changed = false;
        if let Some(e) = enc
            && e != t.doc.encoding
        {
            t.doc.encoding = e;
            changed = true;
        }
        if let Some(e) = eol
            && (e != t.doc.eol || t.doc.mixed)
        {
            t.doc.eol = e;
            t.doc.mixed = false;
            changed = true;
        }
        if changed {
            // the file needs saving to get the new format
            t.doc.rev = rev;
            self.update_title();
            self.update_status();
            invalidate(self.strip_hwnd);
            self.touch();
        }
    }

    fn reopen_as_1251(&mut self) {
        let i = self.active;
        let Some(p) = self.tabs.get(i).and_then(|t| t.doc.path.clone()) else { return };
        if self.tabs[i].doc.dirty() {
            let c = dialogs::ask(
                self.hwnd,
                Icon::Warning,
                "reopen as windows-1251?",
                "the text in this tab is replaced by the file read as windows-1251. ctrl+z brings the current text back.",
                &[(1, "reopen")],
                1,
                true,
            );
            if c != 1 {
                return;
            }
        }
        match files::read(&p) {
            Ok(Ok((bytes, v))) => match files::decode(&bytes, Some(Encoding::Win1251)) {
                Ok(d) => {
                    self.tabs[i].ed.replace_all(&d.text);
                    let rev = self.bump();
                    let doc = &mut self.tabs[i].doc;
                    doc.encoding = Encoding::Win1251;
                    doc.eol = d.eol;
                    doc.mixed = d.mixed;
                    doc.disk = Some(v);
                    doc.rev = rev;
                    self.mark_saved(i, &d.text);
                    self.tabs[i].ed.set_sel(0, 0);
                    self.update_title();
                    self.update_status();
                    invalidate(self.strip_hwnd);
                    self.touch();
                }
                Err(e) => dialogs::problem(self.hwnd, "the file could not be reopened", &e.message()),
            },
            _ => dialogs::problem(self.hwnd, "the file could not be read", &p.display().to_string()),
        }
    }

    fn delete_kept_drafts(&mut self) {
        if self.kept.is_empty() {
            return;
        }
        let c = dialogs::ask(
            self.hwnd,
            Icon::Warning,
            &format!("delete {} kept draft(s)?", self.kept.len()),
            "their text is removed from ryad's folder and cannot be brought back. open tabs are not affected.",
            &[(1, "delete")],
            CANCEL,
            true,
        );
        if c != 1 {
            return;
        }
        for e in self.kept.drain(..) {
            self.purge.push((e.id, 0));
        }
        self.closed.retain(|c| !matches!(c, Closed::Kept(_)));
        self.commit_now();
    }

    fn tab_list_menu(&mut self) {
        // SAFETY: a popup menu destroyed after use.
        unsafe {
            let m = CreatePopupMenu().unwrap_or_default();
            for (i, t) in self.tabs.iter().enumerate() {
                let label = match &t.doc.path {
                    Some(p) => format!(
                        "{}{}\t{}",
                        if t.doc.dirty() { "● " } else { "" },
                        t.doc.title(),
                        p.parent().map(|d| d.display().to_string()).unwrap_or_default()
                    ),
                    None => format!("{}{}", if t.doc.dirty() { "● " } else { "" }, t.doc.title()),
                };
                let l = wide(&label.replace('&', "&&"));
                let flags = if i == self.active { MF_STRING | MF_CHECKED } else { MF_STRING };
                let _ = AppendMenuW(m, flags, (CMD_TAB_BASE as usize) + i, PCWSTR(l.as_ptr()));
            }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTALIGN, pt.x, pt.y, None, self.hwnd, None);
            let _ = DestroyMenu(m);
            if cmd.0 > 0 {
                self.command(cmd.0 as u16);
            }
        }
    }

    fn init_menu(&mut self, menu: HMENU) {
        // SAFETY: menu updates on our own menus.
        unsafe {
            let check = |id: u16, on: bool| {
                CheckMenuItem(menu, id as u32, (if on { MF_CHECKED } else { MF_UNCHECKED }).0);
            };
            let enable = |id: u16, on: bool| {
                let _ = EnableMenuItem(menu, id as u32, if on { MF_ENABLED } else { MF_GRAYED });
            };
            if let Some(t) = self.tabs.get(self.active) {
                check(CMD_WRAP, t.ed.wrap());
                check(CMD_PREVIEW, t.doc.preview);
                for (i, e) in ENCODINGS.iter().enumerate() {
                    check(CMD_ENC_BASE + i as u16, *e == t.doc.encoding);
                }
                for (i, e) in EOLS.iter().enumerate() {
                    check(CMD_EOL_BASE + i as u16, *e == t.doc.eol && !t.doc.mixed);
                }
                enable(CMD_REOPEN_1251, t.doc.path.is_some());
                enable(CMD_UNDO, t.ed.can_undo() || self.focused_field().is_some());
            }
            enable(CMD_REOPEN_CLOSED, !self.closed.is_empty());
            // the drafts submenu and the tab list are rebuilt each time
            let count = GetMenuItemCount(Some(menu));
            for pos in 0..count {
                let id = GetMenuItemID(menu, pos);
                if id == CMD_NEXT_TAB as u32 {
                    // tabs menu: remove old entries after the separator
                    while GetMenuItemCount(Some(menu)) > 3 {
                        let _ = DeleteMenu(menu, 3, MF_BYPOSITION);
                    }
                    for (i, t) in self.tabs.iter().enumerate().take(40) {
                        let l = wide(&format!("&{} {}{}", (i + 1) % 10, if t.doc.dirty() { "● " } else { "" }, t.doc.title().replace('&', "&&")));
                        let flags = if i == self.active { MF_STRING | MF_CHECKED } else { MF_STRING };
                        let _ = AppendMenuW(menu, flags, (CMD_TAB_BASE as usize) + i, PCWSTR(l.as_ptr()));
                    }
                    return;
                }
            }
            // the drafts submenu is the one with no item ids of ours yet or
            // whose first id is a draft or the delete command
            let first = GetMenuItemID(menu, 0);
            let is_drafts = count == 0
                || first == CMD_DELETE_DRAFTS as u32
                || (CMD_DRAFT_BASE as u32..CMD_DRAFT_BASE as u32 + 1000).contains(&first)
                || first == u32::MAX - 1;
            if is_drafts && GetMenuItemID(menu, 0) != CMD_NEW as u32 {
                while GetMenuItemCount(Some(menu)) > 0 {
                    let _ = DeleteMenu(menu, 0, MF_BYPOSITION);
                }
                if self.kept.is_empty() {
                    let l = wide("no kept drafts");
                    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, (u32::MAX - 1) as usize, PCWSTR(l.as_ptr()));
                } else {
                    for (k, e) in self.kept.iter().enumerate().rev().take(30) {
                        let when = format_age(e.when);
                        let title = if e.title.is_empty() { document::NEW_DRAFT.to_string() } else { e.title.clone() };
                        let l = wide(&format!("{}\t{when}", title.replace('&', "&&")));
                        let _ = AppendMenuW(menu, MF_STRING, (CMD_DRAFT_BASE as usize) + k, PCWSTR(l.as_ptr()));
                    }
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                    let l = wide("delete all kept drafts…");
                    let _ = AppendMenuW(menu, MF_STRING, CMD_DELETE_DRAFTS as usize, PCWSTR(l.as_ptr()));
                }
            }
        }
    }

    // ---- notifications from the editors -----------------------------------

    fn on_change(&mut self, hwnd: HWND) {
        if self.loading {
            return;
        }
        let rev = self.bump();
        let Some(i) = self.tabs.iter().position(|t| t.ed.hwnd == hwnd) else { return };
        let was_dirty = self.tabs[i].doc.dirty();
        let old_title = self.tabs[i].doc.title();
        let t = &mut self.tabs[i];
        t.doc.rev = rev;
        // undone back to the saved text? (small documents only)
        let len = t.ed.len();
        if len == t.doc.saved_len && len < SAVEPOINT_LIMIT && files::fnv64(t.ed.text().as_bytes()) == t.doc.saved_hash {
            t.doc.saved_rev = rev;
        }
        if t.doc.path.is_none() {
            t.doc.draft_title = document::draft_title(&t.ed.range(0, 400));
        }
        // the line count for the gutter width; cheap once the control has
        // laid out a small text, so only for those (see gutter_width)
        let old_digits = digits(t.doc.lines);
        if len < SAVEPOINT_LIMIT {
            t.doc.lines = t.ed.line_count();
        }
        if self.tabs[i].doc.preview && i == self.active {
            // SAFETY: our own timer, restarted on every change.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_PREVIEW, PREVIEW_DELAY, None);
            }
        }
        if was_dirty != self.tabs[i].doc.dirty() || old_title != self.tabs[i].doc.title() || old_digits != digits(self.tabs[i].doc.lines) {
            self.update_title();
            self.layout();
        }
        self.touch();
        self.on_view();
    }

    fn on_close_request(&mut self) {
        if self.closing {
            return;
        }
        loop {
            self.commit_now();
            match self.writer.flush() {
                Ok(_) => break,
                Err(e) => {
                    let c = dialogs::ask(
                        self.hwnd,
                        Icon::Error,
                        "ryad could not save your unsaved text",
                        &format!("{e}\n\nretry, save the text of the current tab to a file, or exit and lose the changes made since the last successful save."),
                        &[(1, "retry"), (2, "save as…"), (3, "exit anyway")],
                        1,
                        true,
                    );
                    match c {
                        1 => continue,
                        2 => {
                            self.save(self.active, true);
                            continue;
                        }
                        3 => break,
                        _ => return,
                    }
                }
            }
        }
        self.closing = true;
        let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        // SAFETY: our own window.
        unsafe {
            let _ = GetWindowPlacement(self.hwnd, &mut wp);
        }
        let r = wp.rcNormalPosition;
        let s = Settings { window: Some([r.left, r.top, r.right, r.bottom]), maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32, split: Some(self.split) };
        let _ = settings::save(&self.settings_path, &s);
        // each editor releases its tom interface before its control is
        // destroyed, while the main window still exists (Editor::drop).
        self.tabs.clear();
        // SAFETY: ends the message loop via WM_DESTROY.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn on_dpi(&mut self, dpi: u32, suggested: &RECT) {
        self.dpi = dpi.max(96);
        // SAFETY: screen dc for the font queries.
        unsafe {
            let dc = GetDC(None);
            self.fonts = ui::make_fonts(dc, self.dpi);
            ReleaseDC(None, dc);
        }
        self.find.set_fonts(&self.fonts);
        self.set_banner_fonts();
        self.preview.apply_theme(&self.theme, self.p(24));
        self.preview_key = (0, 0, 0, 0);
        for t in &self.tabs {
            t.ed.apply_theme(&self.theme, &self.fonts.mono_face, self.fonts.editor_px);
        }
        move_window(self.hwnd, *suggested);
        self.layout();
    }

    fn on_theme(&mut self) {
        let theme = if ui::high_contrast() { ui::system_theme() } else { Theme::current() };
        let accent = ui::accent(theme.ground, theme.dark);
        if theme == self.theme && accent == self.accent {
            return;
        }
        self.theme = theme;
        self.accent = accent;
        self.preview.apply_theme(&self.theme, self.p(24));
        self.preview_key = (0, 0, 0, 0);
        ui::apply_title_bar(self.hwnd, &self.theme);
        // SAFETY: replace our brush.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.edit_brush.0));
            self.edit_brush = CreateSolidBrush(ui::colorref(theme.cap));
        }
        for t in &self.tabs {
            t.ed.apply_theme(&self.theme, &self.fonts.mono_face, self.fonts.editor_px);
        }
        self.layout();
        // SAFETY: repaint everything.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                windows::Win32::Graphics::Gdi::RDW_INVALIDATE | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN | windows::Win32::Graphics::Gdi::RDW_FRAME,
            );
        }
    }

    fn context_menu(&mut self, x: i32, y: i32) {
        // SAFETY: a popup menu destroyed after use.
        unsafe {
            let m = CreatePopupMenu().unwrap_or_default();
            for (id, label) in
                [(CMD_UNDO, "undo"), (0, ""), (CMD_CUT, "cut"), (CMD_COPY, "copy"), (CMD_PASTE, "paste"), (0, ""), (CMD_SELECT_ALL, "select all")]
            {
                if id == 0 {
                    let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
                } else {
                    let l = wide(label);
                    let _ = AppendMenuW(m, MF_STRING, id as usize, PCWSTR(l.as_ptr()));
                }
            }
            let (mut x, mut y) = (x, y);
            if x == -1 && y == -1 {
                let mut pt = POINT::default();
                let _ = GetCaretPos(&mut pt);
                if let Some(t) = self.tabs.get(self.active) {
                    let _ = windows::Win32::Graphics::Gdi::ClientToScreen(t.ed.hwnd, &mut pt);
                }
                x = pt.x;
                y = pt.y;
            }
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD, x, y, None, self.hwnd, None);
            let _ = DestroyMenu(m);
            if cmd.0 > 0 {
                self.command(cmd.0 as u16);
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // SAFETY: our own brush.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.edit_brush.0));
        }
    }
}

fn count_lines(text: &str) -> i32 {
    text.bytes().filter(|&b| b == b'\r').count() as i32 + 1
}

fn digits(n: i32) -> usize {
    n.max(1).to_string().len()
}

fn format_age(when: u64) -> String {
    let age = crate::unix_time().saturating_sub(when);
    match age {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", age / 60),
        3600..=86399 => format!("{} h ago", age / 3600),
        _ => format!("{} days ago", age / 86400),
    }
}

fn sanitize(s: &str) -> String {
    let t: String = s.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let t = t.trim().trim_end_matches('…').trim().to_string();
    if t.is_empty() { "draft".into() } else { t }
}

fn dropped_paths(hdrop: HDROP) -> Vec<PathBuf> {
    // SAFETY: the drop handle is valid during the message; freed below.
    unsafe {
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
        }
        DragFinish(hdrop);
        out
    }
}

fn loword(v: usize) -> u16 {
    (v & 0xffff) as u16
}
fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}
fn point_of(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32)
}

// ---- window procedures ------------------------------------------------------

extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        // SAFETY: default handling before the app exists.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    if a.hwnd != hwnd {
        // SAFETY: as above.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    match msg {
        WM_COMMAND => {
            let code = hiword(wparam.0) as u32;
            let ctl = HWND(lparam.0 as *mut c_void);
            if !ctl.is_invalid() && code == EN_CHANGE {
                a.on_change(ctl);
                return LRESULT(0);
            }
            a.command(loword(wparam.0));
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: windows passes a valid NMHDR (or a larger struct) here.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            match hdr.code {
                EN_SELCHANGE => {
                    let _ = lparam.0 as *const SELCHANGE;
                    a.on_view();
                }
                EN_DROPFILES => {
                    // SAFETY: EN_DROPFILES carries an ENDROPFILES.
                    let d = unsafe { &*(lparam.0 as *const ENDROPFILES) };
                    let paths = dropped_paths(HDROP(d.hDrop.0));
                    a.open_paths(&paths);
                    return LRESULT(1); // handled: the control inserts nothing
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_EDITOR_VIEW => {
            a.on_view();
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let (x, y) = point_of(lparam);
            if a.tabs.get(a.active).is_some_and(|t| t.ed.hwnd.0 == wparam.0 as *mut c_void) {
                a.context_menu(x, y);
                return LRESULT(0);
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_COMMIT => a.commit_now(),
                TIMER_PREVIEW => a.render_preview(),
                TIMER_SCROLL => {
                    // SAFETY: one-shot.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_SCROLL);
                    }
                    if let Some((id, tries)) = a.scroll_retry.take() {
                        // SAFETY: posting to our own window.
                        unsafe {
                            let _ = PostMessageW(Some(hwnd), WM_RESTORE_SCROLL, WPARAM(id as usize), LPARAM(tries));
                        }
                    }
                }
                TIMER_RETRY => {
                    // SAFETY: one-shot.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_RETRY);
                    }
                    if a.session_error.is_some() {
                        a.commit_now();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_RESTORE_SCROLL => {
            let id = wparam.0 as u64;
            if let Some(t) = a.tabs.iter_mut().find(|t| t.doc.id == id)
                && let Some(top) = t.pending_top.take()
            {
                // a first paint makes rich edit lay out at its real size;
                // before that it does not scroll
                // SAFETY: our own control.
                unsafe {
                    let _ = UpdateWindow(t.ed.hwnd);
                }
                t.ed.scroll_to(top);
                // not laid out far enough yet: try again shortly (lparam
                // counts the tries, at most 20 × 50 ms)
                if t.ed.top() < top && lparam.0 < 20 {
                    t.pending_top = Some(top);
                    a.scroll_retry = Some((id, lparam.0 + 1));
                    // SAFETY: our own timer.
                    unsafe {
                        SetTimer(Some(hwnd), TIMER_SCROLL, 50, None);
                    }
                }
                a.view = (-1, -1, -1);
                a.on_view();
            }
            LRESULT(0)
        }
        WM_SESSION_DONE => {
            a.on_session_done();
            LRESULT(0)
        }
        WM_SIZE => {
            a.layout();
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // SAFETY: windows passes a MINMAXINFO here.
            let mm = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            mm.ptMinTrackSize = POINT { x: a.p(420), y: a.p(260) };
            LRESULT(0)
        }
        WM_SETFOCUS => {
            let target = if a.last_focus.is_invalid() { a.tabs.get(a.active).map(|t| t.ed.hwnd) } else { Some(a.last_focus) };
            if let Some(t) = target {
                // SAFETY: focus to one of our controls.
                unsafe {
                    let _ = SetFocus(Some(t));
                }
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let active = loword(wparam.0) != 0;
            if !active {
                // SAFETY: plain query.
                a.last_focus = unsafe { GetFocus() };
            }
            a.window_active = active;
            a.menubar.alt_alone = false;
            invalidate(a.strip_hwnd);
            // SAFETY: default handling (sets focus on activation).
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MEASUREITEM => {
            // SAFETY: windows passes a MEASUREITEMSTRUCT here.
            let mis = unsafe { &mut *(lparam.0 as *mut windows::Win32::UI::Controls::MEASUREITEMSTRUCT) };
            if mis.CtlType == windows::Win32::UI::Controls::ODT_MENU {
                // SAFETY: screen dc for text measurement only.
                unsafe {
                    let dc = GetDC(None);
                    crate::menubar::measure(mis, dc, &a.fonts, a.dpi);
                    ReleaseDC(None, dc);
                }
                return LRESULT(1);
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DRAWITEM => {
            // SAFETY: windows passes a DRAWITEMSTRUCT here.
            let dis = unsafe { &*(lparam.0 as *const windows::Win32::UI::Controls::DRAWITEMSTRUCT) };
            if dis.CtlType == windows::Win32::UI::Controls::ODT_MENU {
                crate::menubar::draw(dis, &a.theme, &a.fonts, a.dpi);
                return LRESULT(1);
            }
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MENUCHAR => crate::menubar::menu_char(loword(wparam.0), HMENU(lparam.0 as *mut c_void)),
        WM_MENUSELECT => {
            crate::menubar::on_menu_select(wparam, lparam);
            LRESULT(0)
        }
        WM_INITMENUPOPUP => {
            a.init_menu(HMENU(wparam.0 as *mut c_void));
            crate::menubar::theme_menu(HMENU(wparam.0 as *mut c_void), &a.theme);
            LRESULT(0)
        }
        WM_DROPFILES => {
            let paths = dropped_paths(HDROP(wparam.0 as *mut c_void));
            a.open_paths(&paths);
            LRESULT(0)
        }
        WM_COPYDATA => {
            // SAFETY: windows passes a COPYDATASTRUCT valid for this call.
            let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if cds.dwData != crate::ipc::COPYDATA_PATHS || a.closing {
                return LRESULT(0);
            }
            // SAFETY: cbData bytes at lpData, utf-16.
            let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let paths = crate::ipc::decode_paths(units);
            trace!("ipc: {} path(s) from another start", paths.len());
            // after returning, so the sender is not kept waiting on dialogs
            let boxed = Box::new(paths);
            // SAFETY: the box is reclaimed in the WM_APP+2 handler.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP + 2, WPARAM(0), LPARAM(Box::into_raw(boxed) as isize));
            }
            LRESULT(1)
        }
        x if x == WM_APP + 2 => {
            // SAFETY: the pointer came from Box::into_raw in WM_COPYDATA.
            let paths = unsafe { Box::from_raw(lparam.0 as *mut Vec<PathBuf>) };
            // SAFETY: bring the window forward (the sender allowed it).
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            if !paths.is_empty() {
                a.open_paths(&paths);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // SAFETY: windows passes the suggested rect here.
            let r = unsafe { *(lparam.0 as *const RECT) };
            a.on_dpi(hiword(wparam.0) as u32, &r);
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => {
            a.on_theme();
            // SAFETY: default handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CTLCOLOREDIT => a.find.edit_colors(windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut c_void), &a.theme, a.edit_brush),
        WM_QUERYENDSESSION => {
            a.commit_now();
            let _ = a.writer.flush();
            LRESULT(1)
        }
        WM_CLOSE => {
            a.on_close_request();
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

extern "system" fn strip_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_PAINT => {
            let p = Paint::begin(hwnd);
            let infos = a.tab_infos();
            a.strip.paint(&p, &a.theme, &a.fonts, a.dpi, &infos, a.active, a.window_active);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = point_of(lparam);
            let hit = a.strip.hit(a.dpi, x, y);
            if hit != a.strip.hover {
                let tab_changed = tab_of(hit) != tab_of(a.strip.hover);
                a.strip.hover = hit;
                invalidate(hwnd);
                if tab_changed {
                    // SAFETY: our tooltip: hide so the next text is asked for
                    unsafe {
                        SendMessageW(a.tooltip, TTM_POP, None, None);
                    }
                }
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                // SAFETY: plain request for WM_MOUSELEAVE.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            a.strip.hover = None;
            a.strip.pressed = None;
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point_of(lparam);
            let hit = a.strip.hit(a.dpi, x, y);
            a.strip.pressed = hit;
            if let Some(Hit::Tab(i)) = hit {
                a.activate(i);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = point_of(lparam);
            let hit = a.strip.hit(a.dpi, x, y);
            let pressed = a.strip.pressed.take();
            if hit == pressed {
                match hit {
                    Some(Hit::Close(i)) => {
                        a.close_tab(i);
                    }
                    Some(Hit::New) => a.new_draft(),
                    Some(Hit::List) => a.tab_list_menu(),
                    _ => {}
                }
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = point_of(lparam);
            if a.strip.hit(a.dpi, x, y).is_none() {
                a.new_draft();
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            let (x, y) = point_of(lparam);
            if let Some(Hit::Tab(i) | Hit::Close(i)) = a.strip.hit(a.dpi, x, y) {
                a.close_tab(i);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = hiword(wparam.0) as i16;
            if a.tabs.len() > 1 {
                let n = a.tabs.len();
                a.activate(if delta < 0 { (a.active + 1).min(n - 1) } else { a.active.saturating_sub(1) });
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: windows passes a valid NMHDR.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.code == TTN_GETDISPINFOW {
                // SAFETY: TTN_GETDISPINFOW carries an NMTTDISPINFOW.
                let info = unsafe { &mut *(lparam.0 as *mut NMTTDISPINFOW) };
                let text = match tab_of(a.strip.hover) {
                    Some(i) => a.tab_tooltip(i),
                    None => match a.strip.hover {
                        Some(Hit::New) => "new draft (ctrl+n)".into(),
                        Some(Hit::List) => "all tabs".into(),
                        _ => String::new(),
                    },
                };
                TIP.with(|t| {
                    *t.borrow_mut() = wide(&text);
                    info.lpszText = windows::core::PWSTR(t.borrow_mut().as_mut_ptr());
                });
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

thread_local! {
    static TIP: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn tab_of(h: Option<Hit>) -> Option<usize> {
    match h {
        Some(Hit::Tab(i) | Hit::Close(i)) => Some(i),
        _ => None,
    }
}

extern "system" fn status_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match (msg, app()) {
        (WM_PAINT, Some(a)) => {
            a.paint_status(hwnd);
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// the bar between the text and the preview: drag to resize, double click
/// for half and half.
/// the menu bar row: "file edit view tabs", opened with the mouse or keys.
extern "system" fn menubar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_PAINT => {
            let p = Paint::begin(hwnd);
            a.menubar.paint(&p, &a.theme, &a.fonts);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = point_of(lparam);
            let hit = a.menubar.hit(x, y);
            if hit != a.menubar.hover {
                a.menubar.hover = hit;
                invalidate(hwnd);
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                // SAFETY: plain request for WM_MOUSELEAVE.
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            a.menubar.hover = None;
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = point_of(lparam);
            if let Some(i) = a.menubar.hit(x, y) {
                a.menubar.hover = None;
                let owner = a.hwnd;
                a.menubar.track(hwnd, owner, i, false);
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

extern "system" fn split_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_PAINT => {
            let p = Paint::begin(hwnd);
            p.fill(p.rect, a.theme.ground);
            let x = p.rect.right / 2;
            p.fill(rect(x, 0, x + 1, p.rect.bottom), if a.splitting { a.theme.ink3 } else { a.theme.hair });
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            a.splitting = true;
            // SAFETY: mouse capture for the drag, released on button up.
            unsafe {
                SetCapture(hwnd);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE if a.splitting => {
            let mut pt = POINT::default();
            let mut rc = RECT::default();
            // SAFETY: plain cursor and geometry queries.
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(a.hwnd, &mut pt);
                let _ = GetClientRect(a.hwnd, &mut rc);
            }
            let room = (rc.right - a.gutter_w).max(1) as f32;
            a.split = ((pt.x - a.gutter_w) as f32 / room).clamp(0.2, 0.8);
            a.layout();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            a.splitting = false;
            // SAFETY: ends the drag started above.
            unsafe {
                let _ = ReleaseCapture();
            }
            invalidate(hwnd);
            a.render_preview();
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            a.split = 0.5;
            a.layout();
            a.render_preview();
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

extern "system" fn gutter_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match (msg, app()) {
        (WM_PAINT, Some(a)) => {
            a.paint_gutter(hwnd);
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        (WM_LBUTTONDOWN, Some(a)) => {
            a.focus_editor();
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

extern "system" fn banner_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match (msg, app()) {
        (WM_PAINT, Some(a)) => {
            let p = Paint::begin(hwnd);
            p.fill(p.rect, a.theme.cap);
            p.fill(rect(0, p.rect.bottom - 1, p.rect.right, p.rect.bottom), a.theme.hair);
            if let Some((text, buttons)) = a.banner_content() {
                let mut right = p.rect.right - a.p(12);
                // SAFETY: plain geometry query.
                for b in a.banner_buttons.iter().take(buttons.len()) {
                    let mut r = RECT::default();
                    unsafe {
                        let _ = GetWindowRect(*b, &mut r);
                        let mut pt = POINT { x: r.left, y: r.top };
                        let _ = ScreenToClient(hwnd, &mut pt);
                        right = right.min(pt.x - a.p(8));
                    }
                }
                let color = if a.session_error.is_some() { a.theme.alert } else { a.theme.ink };
                p.text(&text, rect(a.p(12), 0, right, p.rect.bottom), a.fonts.small, color, Align::Left);
            }
            LRESULT(0)
        }
        (WM_ERASEBKGND, _) => LRESULT(1),
        (WM_COMMAND, Some(a)) => {
            if hiword(wparam.0) as u32 == BN_CLICKED {
                a.command(loword(wparam.0));
            }
            LRESULT(0)
        }
        (WM_NOTIFY, Some(a)) => {
            // SAFETY: windows passes a valid NMHDR.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.code == NM_CUSTOMDRAW {
                // SAFETY: NM_CUSTOMDRAW from a button carries an NMCUSTOMDRAW.
                let cd = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                return button_draw(a, cd, hdr.hwndFrom);
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn button_draw(a: &App, cd: &NMCUSTOMDRAW, from: HWND) -> LRESULT {
    use windows::Win32::UI::Controls::{CDDS_PREPAINT, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT};
    if cd.dwDrawStage != CDDS_PREPAINT {
        return LRESULT(CDRF_DODEFAULT as isize);
    }
    let label = find::text_of(from);
    let st = cd.uItemState;
    ui::paint_button(cd.hdc, cd.rc, &label, a.fonts.small, &a.theme, st.contains(CDIS_HOT), st.contains(CDIS_SELECTED), st.contains(CDIS_FOCUS), false);
    LRESULT(CDRF_SKIPDEFAULT as isize)
}

extern "system" fn find_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(a) = app() else {
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_PAINT => {
            let p = Paint::begin(hwnd);
            // SAFETY: plain query.
            let focus = unsafe { GetFocus() };
            a.find.paint(&p, &a.theme, &a.fonts, a.dpi, focus);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_COMMAND => {
            let id = loword(wparam.0);
            let code = hiword(wparam.0) as u32;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            match (id, code) {
                // enter and esc inside the panel (IsDialogMessage)
                (1, _) => match a.find.mode {
                    Mode::GoTo => a.go_to_line(),
                    _ => {
                        // SAFETY: plain query.
                        let in_replace = unsafe { GetFocus() } == a.find.replace;
                        if in_replace { a.replace_one() } else { a.find_next(!shift) }
                    }
                },
                (2, _) => a.close_find(),
                (find::ID_NEXT, BN_CLICKED) => a.find_next(true),
                (find::ID_PREV, BN_CLICKED) => a.find_next(false),
                (find::ID_CASE, BN_CLICKED) => {
                    a.find.case = !a.find.case;
                    a.find.status.clear();
                    invalidate(a.find.button(find::ID_CASE));
                    invalidate(hwnd);
                }
                (find::ID_REPLACE, BN_CLICKED) => a.replace_one(),
                (find::ID_ALL, BN_CLICKED) => a.replace_all(),
                (find::ID_GO, BN_CLICKED) => a.go_to_line(),
                (find::ID_CLOSE, BN_CLICKED) => a.close_find(),
                (find::ID_FIND_EDIT | find::ID_REPLACE_EDIT, 0x0100 | 0x0200) => invalidate(hwnd), // EN_SETFOCUS / EN_KILLFOCUS: field frame
                (find::ID_FIND_EDIT, 0x0300) if !a.find.status.is_empty() => {
                    a.find.status.clear();
                    invalidate(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: windows passes a valid NMHDR.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.code == NM_CUSTOMDRAW {
                // SAFETY: NM_CUSTOMDRAW from a button carries an NMCUSTOMDRAW.
                let cd = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                return a.find.custom_draw(cd, &a.theme, &a.fonts);
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => a.find.edit_colors(windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut c_void), &a.theme, a.edit_brush),
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
