//! the installer and uninstaller window, in kadr's look: a title, a short
//! text, a raised group of switch rows and a pill button. the rows and
//! buttons are real windows buttons (keyboard, screen readers), drawn with
//! the direct2d canvas through NM_CUSTOMDRAW.
//!
//! ported from kadr (src/installer.rs at 29db81d, MIT, same authors); ryad
//! adds the uninstall pages and the "replace windows notepad" switch.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_INVALIDATE, RedrawWindow,
    ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{CDDS_PREPAINT, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::canvas::{Canvas, Fonts, Painter, frect, make_fonts};
use crate::install::{self, Paths};
use crate::ui::Theme;
use crate::wide;

const CLASS: PCWSTR = w!("ryad.installer");
const ID_NOTEPAD: i32 = 10;
const ID_RUN: i32 = 11;
const ID_OK: i32 = 1; // IDOK: enter
const ID_CANCEL: i32 = 2; // IDCANCEL: esc

const WIDTH: i32 = 480;
const MARGIN: i32 = 24;
const ROW: i32 = 44;
const ROW_NOTE: i32 = 64;
const PAD: i32 = 16;

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Install,
    Uninstall,
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Ask,
    Done,
}

struct Row {
    id: i32,
    label: &'static str,
    note: &'static str,
    rect: RECT,
}

struct State {
    hwnd: HWND,
    canvas: Canvas,
    fonts: Fonts,
    theme: Theme,
    dpi: u32,
    mode: Mode,
    page: Page,
    paths: Paths,
    previous: Option<String>,
    /// what the done page says about notepad
    notepad_done: Option<bool>,
    ran: bool,
    title_rect: RECT,
    text_rect: RECT,
    group: RECT,
    rows: Vec<Row>,
    icons: [HICON; 2],
    code: i32,
}

/// shows the window and returns the process exit code.
pub fn run(mode: Mode) -> i32 {
    let hwnd = match open(mode) {
        Ok(h) => h,
        Err(e) => {
            crate::dialogs::error(None, &format!("could not open the installer\n\n{}", e.message()));
            return 1;
        }
    };
    let mut msg = MSG::default();
    // SAFETY: standard loop with dialog keys (tab, enter, esc, space).
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if IsWindow(Some(hwnd)).as_bool() && IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}

fn open(mode: Mode) -> windows::core::Result<HWND> {
    // SAFETY: class registration and window creation; the state box is
    // owned by the window and freed in WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let title = if mode == Mode::Install { w!("ryad setup") } else { w!("remove ryad") };
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS,
            title,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
            0,
            0,
            WIDTH,
            400,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        let canvas = Canvas::new()?;
        let fonts = make_fonts(&canvas.dwrite, dpi)?;
        let paths = install::paths();
        let mut rows = Vec::new();
        match mode {
            Mode::Install => {
                if install::notepad_present() {
                    rows.push(Row {
                        id: ID_NOTEPAD,
                        label: "replace windows notepad",
                        note: "removes the notepad app for you and opens windows settings to make ryad the default for text files",
                        rect: RECT::default(),
                    });
                }
                rows.push(Row { id: ID_RUN, label: "start ryad now", note: "", rect: RECT::default() });
            }
            Mode::Uninstall => {
                if install::notepad_was_removed(&paths) {
                    rows.push(Row {
                        id: ID_NOTEPAD,
                        label: "bring windows notepad back",
                        note: "ryad removed it at install; this opens its microsoft store page",
                        rect: RECT::default(),
                    });
                }
            }
        }
        let state = State {
            hwnd,
            canvas,
            fonts,
            theme: Theme::current(),
            dpi,
            mode,
            page: Page::Ask,
            previous: install::installed_version(),
            notepad_done: None,
            ran: false,
            title_rect: RECT::default(),
            text_rect: RECT::default(),
            group: RECT::default(),
            rows,
            paths,
            icons: [HICON::default(); 2],
            code: 0,
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(state)) as isize);
        let st = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State);
        st.create_controls();
        st.apply_theme();
        st.layout(true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        if let Some(ok) = st.control(ID_OK) {
            let _ = SetFocus(Some(ok));
        }
        Ok(hwnd)
    }
}

impl State {
    fn px(&self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn control(&self, id: i32) -> Option<HWND> {
        // SAFETY: plain lookup of our own child.
        unsafe { GetDlgItem(Some(self.hwnd), id).ok() }
    }

    fn create_controls(&mut self) {
        let mk = |id: i32, text: &str, style: u32| {
            let t = wide(text);
            // SAFETY: child buttons of our window.
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    PCWSTR(t.as_ptr()),
                    WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                    0,
                    0,
                    10,
                    10,
                    Some(self.hwnd),
                    Some(HMENU(id as isize as *mut c_void)),
                    None,
                    None,
                )
                .unwrap_or_default()
            }
        };
        let check = BS_AUTOCHECKBOX as u32 | BS_NOTIFY as u32;
        for (k, row) in self.rows.iter().enumerate() {
            let h = mk(row.id, row.label, check | if k == 0 { WS_GROUP.0 } else { 0 });
            // on by default: start ryad now; bring notepad back. off: replace notepad.
            let on = row.id == ID_RUN || self.mode == Mode::Uninstall;
            if on {
                // SAFETY: our own check box.
                unsafe {
                    SendMessageW(h, BM_SETCHECK, Some(WPARAM(1)), None);
                }
            }
        }
        mk(ID_OK, self.primary_label(), BS_DEFPUSHBUTTON as u32 | BS_NOTIFY as u32 | WS_GROUP.0);
        mk(ID_CANCEL, "cancel", BS_PUSHBUTTON as u32 | BS_NOTIFY as u32);
    }

    fn primary_label(&self) -> &'static str {
        match (self.mode, self.page, &self.previous) {
            (_, Page::Done, _) => "close",
            (Mode::Uninstall, _, _) => "remove",
            (Mode::Install, _, Some(v)) if v == install::VERSION => "reinstall",
            (Mode::Install, _, Some(_)) => "update",
            (Mode::Install, _, None) => "install",
        }
    }

    fn title(&self) -> String {
        match (self.mode, self.page, &self.previous) {
            (Mode::Uninstall, Page::Ask, _) => "remove ryad".into(),
            (Mode::Uninstall, Page::Done, _) => "ryad is removed".into(),
            (Mode::Install, Page::Done, Some(_)) => "ryad is updated".into(),
            (Mode::Install, Page::Done, None) => "ryad is installed".into(),
            (Mode::Install, Page::Ask, Some(v)) if v == install::VERSION => "reinstall ryad".into(),
            (Mode::Install, Page::Ask, Some(_)) => "update ryad".into(),
            (Mode::Install, Page::Ask, None) => "install ryad".into(),
        }
    }

    fn short(&self, full: &std::path::Path) -> String {
        let home = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from);
        match home.as_ref().and_then(|h| full.strip_prefix(h).ok()) {
            Some(rel) => rel.display().to_string().to_lowercase(),
            None => full.display().to_string(),
        }
    }

    fn text(&self) -> String {
        let dir = self.short(&self.paths.dir);
        let data = self.short(&self.paths.data);
        match (self.mode, self.page, &self.previous) {
            (Mode::Install, Page::Ask, None) => format!(
                "a small editor that keeps your drafts between launches. for you only, in {dir}. no administrator rights, nothing running in the background."
            ),
            (Mode::Install, Page::Ask, Some(old)) => {
                format!("replaces ryad {old} with {}. a running ryad is closed first and keeps its session. your drafts stay.", install::VERSION)
            }
            (Mode::Install, Page::Done, _) => {
                let start = if self.ran { "ryad is open." } else { "start it from the start menu." };
                match self.notepad_done {
                    Some(true) => {
                        format!("{start} windows notepad is removed. windows settings shows ryad's page: choose “set default” to open text files with ryad.")
                    }
                    Some(false) => format!("{start} windows notepad could not be removed; you can do it in windows settings › apps."),
                    None => format!("{start} it is in “open with” for text files; windows settings › apps › default apps can make it the default."),
                }
            }
            (Mode::Uninstall, Page::Ask, _) => format!(
                "removes the program, its start menu entry and its “open with” entries. your drafts and settings stay in {data}, so a later install brings them back."
            ),
            (Mode::Uninstall, Page::Done, _) => format!("your drafts are still in {data}. delete that folder to remove them too."),
        }
    }

    fn checked(&self, id: i32) -> bool {
        // SAFETY: plain query on our own check box.
        self.control(id).is_some_and(|c| unsafe { SendMessageW(c, BM_GETCHECK, None, None).0 == 1 })
    }

    fn apply_theme(&mut self) {
        self.theme = Theme::current();
        crate::ui::apply_title_bar(self.hwnd, &self.theme);
        // SAFETY: our own icons from the resources; the old ones are destroyed.
        unsafe {
            let icon = |size: i32| {
                LoadImageW(GetModuleHandleW(None).ok().map(|m| m.into()), PCWSTR(std::ptr::without_provenance(1)), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                    .map(|h| HICON(h.0))
                    .unwrap_or_default()
            };
            let small = icon(GetSystemMetricsForDpi(SM_CXSMICON, self.dpi));
            let big = icon(GetSystemMetricsForDpi(SM_CXICON, self.dpi));
            SendMessageW(self.hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(small.0 as isize)));
            SendMessageW(self.hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(big.0 as isize)));
            for old in std::mem::replace(&mut self.icons, [small, big]) {
                if !old.is_invalid() {
                    let _ = DestroyIcon(old);
                }
            }
        }
        self.redraw();
    }

    fn redraw(&self) {
        // SAFETY: our own window.
        unsafe {
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    fn layout(&mut self, center: bool) {
        let dpi = self.dpi as i32;
        let p = |v: i32| (v * dpi + 48) / 96;
        let width = p(WIDTH);
        let (left, right) = (p(MARGIN), width - p(MARGIN));
        self.title_rect = RECT { left: left + p(8), top: p(16), right, bottom: p(48) };
        let text = self.text();
        let max = (right - left - p(16)) as f32;
        let (_, h) = self.canvas.measure_box(&text, &self.fonts.para, max);
        self.text_rect = RECT { left: left + p(8), top: p(52), right: right - p(8), bottom: p(52) + h.ceil() as i32 };
        let mut y = self.text_rect.bottom + p(20);
        let asking = self.page == Page::Ask && !self.rows.is_empty();
        if asking {
            let top = y;
            let note_w = (right - left - p(PAD) * 3 - p(36)) as f32;
            for row in &mut self.rows {
                let h = if row.note.is_empty() {
                    p(ROW)
                } else {
                    let (_, nh) = self.canvas.measure_box(row.note, &self.fonts.small_para, note_w);
                    (p(ROW_NOTE) - p(20) + nh.ceil() as i32).max(p(ROW_NOTE))
                };
                row.rect = RECT { left, top: y, right, bottom: y + h };
                y += h;
            }
            self.group = RECT { left, top, right, bottom: y };
            y += p(20);
        }
        let bh = p(32) + 2 * p(4);
        let bw = |label: &str| (self.canvas.measure(label, &self.fonts.body).ceil() as i32 + p(40)).max(p(96)) + 2 * p(4);
        let ok_w = bw(self.primary_label());
        let ok = RECT { left: right + p(4) - ok_w, top: y, right: right + p(4), bottom: y + bh };
        let cancel_w = bw("cancel");
        let cancel = RECT { left: ok.left - cancel_w, top: y, right: ok.left, bottom: y + bh };
        let height = ok.bottom - p(4) + p(MARGIN);
        let put = |id: i32, r: RECT, show: bool| {
            // SAFETY: moving our own children.
            unsafe {
                if let Ok(c) = GetDlgItem(Some(self.hwnd), id) {
                    let _ = SetWindowPos(c, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    let _ = ShowWindow(c, if show { SW_SHOW } else { SW_HIDE });
                }
            }
        };
        for row in &self.rows {
            put(row.id, row.rect, asking);
        }
        put(ID_OK, ok, true);
        put(ID_CANCEL, cancel, self.page == Page::Ask);
        // SAFETY: label, size and place of our own window.
        unsafe {
            if let Some(c) = self.control(ID_OK) {
                let t = wide(self.primary_label());
                let _ = SetWindowTextW(c, PCWSTR(t.as_ptr()));
            }
            let mut r = RECT { left: 0, top: 0, right: width, bottom: height };
            let _ = AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU, false, WINDOW_EX_STYLE(0), self.dpi);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST), &mut info);
            let wa = info.rcWork;
            if center {
                let x = wa.left + ((wa.right - wa.left) - w) / 2;
                let y = wa.top + ((wa.bottom - wa.top) - h) / 3;
                let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            } else {
                let mut now = RECT::default();
                let _ = GetWindowRect(self.hwnd, &mut now);
                let _ = SetWindowPos(self.hwnd, None, now.left, now.top.min(wa.bottom - h).max(wa.top), w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        self.redraw();
    }

    fn paint_window(&self, p: &Painter, client: RECT) {
        let t = self.theme;
        p.fill(client, t.ground);
        p.text(&self.title(), &self.fonts.title, frect(self.title_rect), t.ink);
        p.text(&self.text(), &self.fonts.para, frect(self.text_rect), t.ink2);
        if self.page == Page::Ask {
            for i in 0..self.rows.len() {
                self.paint_row(p, i, false);
            }
        }
    }

    fn paint_row(&self, p: &Painter, i: usize, hot: bool) {
        let t = self.theme;
        let row = &self.rows[i];
        let r = row.rect;
        let radius = self.px(12) as f32;
        let hair = [t.hair[0], t.hair[1], t.hair[2], 1.0];
        p.fill(r, t.ground);
        p.fill_round(self.group, radius, t.raised);
        if hot || self.focused() == Some(row.id) {
            p.clip(r, || p.fill_round(self.group, radius, t.hover));
        }
        p.stroke_round(self.group, radius, hair);
        if i > 0 {
            let y = r.top as f32 + 0.5;
            p.line((r.left as f32 + self.px(PAD) as f32, y), (r.right as f32 - self.px(PAD) as f32, y), hair);
        }
        let switch_w = self.px(36) + self.px(PAD) * 2;
        if row.note.is_empty() {
            p.text(row.label, &self.fonts.body, frect(RECT { left: r.left + self.px(PAD), right: r.right - switch_w, ..r }), t.ink);
        } else {
            let line = RECT { left: r.left + self.px(PAD), top: r.top + self.px(10), right: r.right - switch_w, bottom: r.top + self.px(30) };
            p.text(row.label, &self.fonts.body, frect(line), t.ink);
            let note = RECT { top: line.bottom, bottom: r.bottom - self.px(8), ..line };
            p.text(row.note, &self.fonts.small_para, frect(note), t.ink3);
        }
        self.paint_switch(p, r, self.checked(row.id));
    }

    fn paint_switch(&self, p: &Painter, line: RECT, on: bool) {
        let t = self.theme;
        let (w, h) = (self.px(36), self.px(20));
        let top = line.top + (line.bottom - line.top - h) / 2;
        let r = RECT { left: line.right - self.px(PAD) - w, top, right: line.right - self.px(PAD), bottom: top + h };
        let knob = self.px(12);
        let ky = r.top + (h - knob) / 2;
        if on {
            p.fill_round(r, h as f32 / 2.0, t.ink);
            let k = RECT { left: r.right - self.px(4) - knob, top: ky, right: r.right - self.px(4), bottom: ky + knob };
            p.fill_round(k, knob as f32 / 2.0, t.raised);
        } else {
            p.stroke_round(r, h as f32 / 2.0, [t.ink3[0], t.ink3[1], t.ink3[2], 1.0]);
            let k = RECT { left: r.left + self.px(4), top: ky, right: r.left + self.px(4) + knob, bottom: ky + knob };
            p.fill_round(k, knob as f32 / 2.0, t.ink2);
        }
    }

    fn paint_button(&self, p: &Painter, id: i32, r: RECT, hot: bool, pressed: bool) {
        let t = self.theme;
        p.fill(r, t.ground);
        let inset = self.px(4);
        let pill = RECT { left: r.left + inset, top: r.top + inset, right: r.right - inset, bottom: r.bottom - inset };
        let label = if id == ID_OK { self.primary_label() } else { "cancel" };
        let radius = self.px(8) as f32;
        if id == ID_OK {
            p.fill_round(pill, radius, if pressed { t.ink2 } else { t.ink });
            p.text_center(label, &self.fonts.body, frect(pill), t.ground);
        } else {
            p.fill_round(pill, radius, if pressed || hot { t.hover } else { t.cap });
            p.stroke_round(pill, radius, [t.hair[0], t.hair[1], t.hair[2], 1.0]);
            p.text_center(label, &self.fonts.body, frect(pill), t.ink);
        }
        if self.focused() == Some(id) {
            let ring = RECT { left: r.left + self.px(1), top: r.top + self.px(1), right: r.right - self.px(1), bottom: r.bottom - self.px(1) };
            p.stroke_round(ring, radius + self.px(3) as f32, [t.ink[0], t.ink[1], t.ink[2], 0.5]);
        }
    }

    fn focused(&self) -> Option<i32> {
        // SAFETY: plain query.
        let f = unsafe { GetFocus() };
        self.rows.iter().map(|r| r.id).chain([ID_OK, ID_CANCEL]).find(|id| self.control(*id) == Some(f))
    }

    fn command(&mut self, id: i32, code: u32) {
        match (id, code) {
            (ID_OK, c) if c == BN_CLICKED => match self.page {
                Page::Ask => self.go(),
                Page::Done => self.close(),
            },
            (ID_CANCEL, c) if c == BN_CLICKED => self.close(),
            (_, c) if c == BN_CLICKED || c == BN_SETFOCUS || c == BN_KILLFOCUS => self.redraw(),
            _ => {}
        }
    }

    fn enable_all(&self, on: bool) {
        for id in self.rows.iter().map(|r| r.id).chain([ID_OK, ID_CANCEL]) {
            if let Some(c) = self.control(id) {
                // SAFETY: our own child.
                unsafe {
                    let _ = EnableWindow(c, on);
                }
            }
        }
    }

    fn go(&mut self) {
        self.enable_all(false);
        // SAFETY: a wait cursor while files and registry are written.
        unsafe {
            if let Ok(wait) = LoadCursorW(None, IDC_WAIT) {
                SetCursor(Some(wait));
            }
        }
        let notepad = self.rows.iter().any(|r| r.id == ID_NOTEPAD) && self.checked(ID_NOTEPAD);
        let result = match self.mode {
            Mode::Install => install::install(&self.paths).map(|()| {
                if notepad {
                    self.notepad_done = Some(install::replace_notepad(&self.paths).is_ok());
                }
                if self.checked(ID_RUN) {
                    // SAFETY: let the new ryad take the foreground.
                    unsafe {
                        let _ = AllowSetForegroundWindow(ASFW_ANY);
                    }
                    self.ran = std::process::Command::new(&self.paths.exe).spawn().is_ok();
                }
                if self.notepad_done == Some(true) {
                    install::open_default_apps();
                }
            }),
            Mode::Uninstall => install::uninstall(&self.paths, notepad),
        };
        self.enable_all(true);
        match result {
            Ok(()) => {
                self.page = Page::Done;
                self.layout(false);
                if let Some(ok) = self.control(ID_OK) {
                    // SAFETY: focus to the close button.
                    unsafe {
                        let _ = SetFocus(Some(ok));
                    }
                }
            }
            Err(e) => {
                self.code = 1;
                let what = if self.mode == Mode::Install { "ryad could not be installed" } else { "ryad could not be removed completely" };
                crate::dialogs::problem(self.hwnd, what, &e);
            }
        }
    }

    fn close(&self) {
        // SAFETY: ends the loop via WM_DESTROY.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        for icon in self.icons {
            if !icon.is_invalid() {
                // SAFETY: our own icons.
                unsafe {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    }
}

fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    // SAFETY: the pointer was stored by open() and is cleared in WM_NCDESTROY.
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(st) = state(hwnd) else {
        // SAFETY: default handling before the state exists.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // SAFETY: standard handling of our own window's messages.
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                st.canvas.paint(hdc, (client.right, client.bottom), (0, 0), |p| st.paint_window(p, client));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_NOTIFY => {
                let hdr = &*(lparam.0 as *const NMHDR);
                if hdr.code != NM_CUSTOMDRAW {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let cd = &*(lparam.0 as *const NMCUSTOMDRAW);
                if cd.dwDrawStage != CDDS_PREPAINT {
                    return LRESULT(CDRF_DODEFAULT as isize);
                }
                let hot = (cd.uItemState.0 & CDIS_HOT.0) != 0;
                let pressed = (cd.uItemState.0 & CDIS_SELECTED.0) != 0;
                let mut wr = RECT::default();
                let _ = GetWindowRect(hdr.hwndFrom, &mut wr);
                let mut origin = POINT { x: wr.left, y: wr.top };
                let _ = ScreenToClient(hwnd, &mut origin);
                let size = (cd.rc.right - cd.rc.left, cd.rc.bottom - cd.rc.top);
                let r = RECT { left: origin.x, top: origin.y, right: origin.x + size.0, bottom: origin.y + size.1 };
                let id = hdr.idFrom as i32;
                let row = st.rows.iter().position(|x| x.id == id);
                st.canvas.paint(cd.hdc, size, (origin.x, origin.y), |p| match row {
                    Some(i) => st.paint_row(p, i, hot),
                    None => st.paint_button(p, id, r, hot, pressed),
                });
                LRESULT(CDRF_SKIPDEFAULT as isize)
            }
            WM_COMMAND => {
                st.command((wparam.0 & 0xffff) as i32, ((wparam.0 >> 16) & 0xffff) as u32);
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                st.apply_theme();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                st.dpi = ((wparam.0 & 0xffff) as u32).max(96);
                if let Ok(f) = make_fonts(&st.canvas.dwrite, st.dpi) {
                    st.fonts = f;
                }
                let s = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, s.left, s.top, s.right - s.left, s.bottom - s.top, SWP_NOZORDER | SWP_NOACTIVATE);
                st.apply_theme();
                st.layout(false);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(st.code);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
