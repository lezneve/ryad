//! the find panel: a narrow strip above the status line for find, replace
//! and go to line. real edit fields and buttons (keyboard, ime, screen
//! readers), painted in ryad's theme. esc closes it and returns focus to the
//! text; enter is "next" (shift+enter "previous"), or "go" for a line.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{HBRUSH, HDC, SetBkColor, SetTextColor};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{CDDS_PREPAINT, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, NMCUSTOMDRAW};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_NOTIFY, BS_PUSHBUTTON, CreateWindowExW, ES_AUTOHSCROLL, ES_NUMBER, GWL_STYLE, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HMENU,
    MoveWindow, SW_HIDE, SW_SHOW, SendMessageW, SetWindowLongPtrW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD,
    WS_CLIPSIBLINGS, WS_EX_CONTROLPARENT, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::ui::{self, Align, Fonts, Paint, Theme, px, rect};
use crate::wide;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Find,
    Replace,
    GoTo,
}

pub const ID_FIND_EDIT: u16 = 301;
pub const ID_REPLACE_EDIT: u16 = 302;
pub const ID_PREV: u16 = 303;
pub const ID_NEXT: u16 = 304;
pub const ID_CASE: u16 = 305;
pub const ID_REPLACE: u16 = 306;
pub const ID_ALL: u16 = 307;
pub const ID_CLOSE: u16 = 308;
pub const ID_GO: u16 = 309;

pub struct Panel {
    pub hwnd: HWND,
    pub find: HWND,
    pub replace: HWND,
    buttons: Vec<(u16, HWND)>,
    pub mode: Mode,
    pub open: bool,
    pub case: bool,
    /// the last result, shown at the right: "no matches", "replaced 3"…
    pub status: String,
    pub line_hint: String,
    labels: [RECT; 2],
}

fn child(parent: HWND, class: PCWSTR, text: &str, style: u32, id: u16) -> HWND {
    let t = wide(text);
    // SAFETY: child control of our own panel.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(t.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_CLIPSIBLINGS.0 | style),
            0,
            0,
            10,
            10,
            Some(parent),
            Some(HMENU(id as usize as *mut c_void)),
            GetModuleHandleW(None).ok().map(|m| m.into()),
            None,
        )
        .unwrap_or_default()
    }
}

impl Panel {
    pub fn create(parent: HWND, class: PCWSTR) -> Panel {
        // SAFETY: our own registered class; WS_EX_CONTROLPARENT lets tab move
        // between the panel's controls and the rest of the window.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class,
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_CLIPSIBLINGS.0),
                0,
                0,
                10,
                10,
                Some(parent),
                None,
                GetModuleHandleW(None).ok().map(|m| m.into()),
                None,
            )
            .unwrap_or_default()
        };
        let edit = WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32;
        let find = child(hwnd, w!("EDIT"), "", edit, ID_FIND_EDIT);
        let replace = child(hwnd, w!("EDIT"), "", edit, ID_REPLACE_EDIT);
        let b = WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32 | BS_NOTIFY as u32;
        let buttons = [
            (ID_PREV, "previous"),
            (ID_NEXT, "next"),
            (ID_CASE, "match case"),
            (ID_REPLACE, "replace"),
            (ID_ALL, "replace all"),
            (ID_GO, "go"),
            (ID_CLOSE, "×"),
        ]
        .iter()
        .map(|(id, label)| (*id, child(hwnd, w!("BUTTON"), label, b, *id)))
        .collect();
        Panel {
            hwnd,
            find,
            replace,
            buttons,
            mode: Mode::Find,
            open: false,
            case: false,
            status: String::new(),
            line_hint: String::new(),
            labels: [RECT::default(); 2],
        }
    }

    pub fn height(&self, dpi: u32) -> i32 {
        if !self.open {
            0
        } else if self.mode == Mode::Replace {
            px(dpi, 72)
        } else {
            px(dpi, 40)
        }
    }

    pub fn set_fonts(&self, fonts: &Fonts) {
        for h in [self.find, self.replace] {
            // SAFETY: our controls; the font outlives them (replaced together).
            unsafe {
                SendMessageW(h, WM_SETFONT, Some(WPARAM(fonts.mono.0 as usize)), Some(LPARAM(1)));
            }
        }
        for (_, h) in &self.buttons {
            // SAFETY: as above.
            unsafe {
                SendMessageW(*h, WM_SETFONT, Some(WPARAM(fonts.small.0 as usize)), Some(LPARAM(1)));
            }
        }
    }

    pub fn button(&self, id: u16) -> HWND {
        self.buttons.iter().find(|(i, _)| *i == id).map(|(_, h)| *h).unwrap_or_default()
    }

    /// switches mode; the go-to-line field only takes digits.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.status.clear();
        // SAFETY: style bits on our own edit control.
        unsafe {
            let style = GetWindowLongPtrW(self.find, GWL_STYLE);
            let style = if mode == Mode::GoTo { style | ES_NUMBER as isize } else { style & !(ES_NUMBER as isize) };
            SetWindowLongPtrW(self.find, GWL_STYLE, style);
        }
        let show = |id: u16, on: bool| {
            // SAFETY: our own child.
            unsafe {
                let _ = ShowWindow(self.button(id), if on { SW_SHOW } else { SW_HIDE });
            }
        };
        let find = mode != Mode::GoTo;
        show(ID_PREV, find);
        show(ID_NEXT, find);
        show(ID_CASE, find);
        show(ID_REPLACE, mode == Mode::Replace);
        show(ID_ALL, mode == Mode::Replace);
        show(ID_GO, !find);
        // SAFETY: our own child.
        unsafe {
            let _ = ShowWindow(self.replace, if mode == Mode::Replace { SW_SHOW } else { SW_HIDE });
        }
    }

    pub fn layout(&mut self, dpi: u32, x: i32, y: i32, width: i32, dc: HDC, fonts: &Fonts) {
        let p = |v| px(dpi, v);
        let h = self.height(dpi);
        // SAFETY: moving our own windows.
        unsafe {
            let _ = MoveWindow(self.hwnd, x, y, width, h, true);
        }
        if !self.open {
            return;
        }
        let row = p(40);
        let field_h = p(26);
        let label_w = p(60);
        let field_w = p(260).min(width - p(400)).max(p(120));
        let place = |hw: HWND, l: i32, t: i32, w: i32, hh: i32| {
            // SAFETY: as above.
            unsafe {
                let _ = MoveWindow(hw, l, t, w, hh, true);
            }
        };
        let mut lx = p(12);
        self.labels[0] = rect(lx, 0, lx + label_w, row);
        lx += label_w;
        let fy = (row - field_h) / 2;
        place(self.find, lx + p(6), fy + p(4), field_w - p(12), field_h - p(8));
        let mut bx = lx + field_w + p(8);
        let bw = |label: &str| ui::text_width(dc, label, fonts.small) + p(24);
        let order: &[(u16, &str)] =
            if self.mode == Mode::GoTo { &[(ID_GO, "go")] } else { &[(ID_PREV, "previous"), (ID_NEXT, "next"), (ID_CASE, "match case")] };
        for (id, label) in order {
            let w = bw(label);
            place(self.button(*id), bx, fy, w, field_h);
            bx += w + p(4);
        }
        place(self.button(ID_CLOSE), width - p(12) - field_h, fy, field_h, field_h);
        if self.mode == Mode::Replace {
            let y2 = row - p(8);
            self.labels[1] = rect(p(12), y2, p(12) + label_w, y2 + row);
            place(self.replace, lx + p(6), y2 + fy + p(4), field_w - p(12), field_h - p(8));
            let mut bx = lx + field_w + p(8);
            for (id, label) in [(ID_REPLACE, "replace"), (ID_ALL, "replace all")] {
                let w = bw(label);
                place(self.button(id), bx, y2 + fy, w, field_h);
                bx += w + p(4);
            }
        }
    }

    /// the field frames are painted by the panel behind the edit controls.
    fn field_rect(&self, edit: HWND, dpi: u32) -> RECT {
        let mut r = RECT::default();
        // SAFETY: plain geometry queries on our own windows.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(edit, &mut r);
            let mut pts = [windows::Win32::Foundation::POINT { x: r.left, y: r.top }, windows::Win32::Foundation::POINT { x: r.right, y: r.bottom }];
            windows::Win32::Graphics::Gdi::MapWindowPoints(None, Some(self.hwnd), &mut pts);
            r = rect(pts[0].x, pts[0].y, pts[1].x, pts[1].y);
        }
        let p = |v| px(dpi, v);
        rect(r.left - p(6), r.top - p(4), r.right + p(6), r.bottom + p(4))
    }

    pub fn paint(&self, p: &Paint, theme: &Theme, fonts: &Fonts, dpi: u32, focus: HWND) {
        p.fill(p.rect, theme.ground);
        p.fill(rect(0, 0, p.rect.right, 1), theme.hair);
        let first = match self.mode {
            Mode::GoTo => "line",
            _ => "find",
        };
        p.text(first, self.labels[0], fonts.small, theme.ink2, Align::Left);
        let mut fields = vec![self.find];
        if self.mode == Mode::Replace {
            p.text("replace", self.labels[1], fonts.small, theme.ink2, Align::Left);
            fields.push(self.replace);
        }
        for f in fields {
            let r = self.field_rect(f, dpi);
            p.fill(r, theme.cap);
            ui::frame(p.dc, r, if focus == f { theme.ink2 } else { theme.hair });
        }
        // status at the right, before the close button
        let right = p.rect.right - px(dpi, 52);
        let left = right - px(dpi, 240);
        let text = if self.mode == Mode::GoTo && self.status.is_empty() { self.line_hint.as_str() } else { self.status.as_str() };
        p.text(text, rect(left, 0, right, px(dpi, 40)), fonts.small, theme.ink2, Align::Right);
    }

    /// NM_CUSTOMDRAW for the panel's buttons.
    pub fn custom_draw(&self, cd: &NMCUSTOMDRAW, theme: &Theme, fonts: &Fonts) -> LRESULT {
        if cd.dwDrawStage != CDDS_PREPAINT {
            return LRESULT(CDRF_DODEFAULT as isize);
        }
        let id = cd.hdr.idFrom as u16;
        let label = match id {
            ID_PREV => "previous",
            ID_NEXT => "next",
            ID_CASE => "match case",
            ID_REPLACE => "replace",
            ID_ALL => "replace all",
            ID_GO => "go",
            ID_CLOSE => "×",
            _ => return LRESULT(CDRF_DODEFAULT as isize),
        };
        let st = cd.uItemState;
        ui::paint_button(
            cd.hdc,
            cd.rc,
            label,
            fonts.small,
            theme,
            st.contains(CDIS_HOT),
            st.contains(CDIS_SELECTED),
            st.contains(CDIS_FOCUS),
            id == ID_CASE && self.case,
        );
        LRESULT(CDRF_SKIPDEFAULT as isize)
    }

    /// colours for the edit fields (WM_CTLCOLOREDIT).
    pub fn edit_colors(&self, dc: HDC, theme: &Theme, brush: HBRUSH) -> LRESULT {
        // SAFETY: plain dc state for the control's own paint.
        unsafe {
            SetTextColor(dc, ui::colorref(theme.ink));
            SetBkColor(dc, ui::colorref(theme.cap));
        }
        LRESULT(brush.0 as isize)
    }
}

pub fn text_of(h: HWND) -> String {
    // SAFETY: buffer sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got as usize])
    }
}

pub fn set_text(h: HWND, s: &str) {
    let t = wide(s);
    // SAFETY: valid string.
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}
