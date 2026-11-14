//! the markdown preview at the right of the text: a read-only rich edit
//! control in rich text mode, filled with the rtf from markdown.rs. one
//! control for the window; it shows the active tab when that tab has the
//! preview on. the text can be selected and copied; links are not opened.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::RichEdit::{
    EM_GETLANGOPTIONS, EM_GETSCROLLPOS, EM_SETBKGNDCOLOR, EM_SETEVENTMASK, EM_SETLANGOPTIONS, EM_SETSCROLLPOS, EM_SETTARGETDEVICE, EM_SETTEXTEX,
    EM_SETUNDOLIMIT, MSFTEDIT_CLASS, SETTEXTEX,
};
use windows::Win32::UI::Controls::{EM_SETMARGINS, SetWindowTheme};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GetScrollInfo, SB_VERT, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SendMessageW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_CLIPSIBLINGS, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use crate::ui::Theme;

/// texts above this many utf-16 units are not previewed (rtf building and
/// rich edit layout would make typing in them sluggish)
pub const MAX_UNITS: usize = 1024 * 1024;

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    // SAFETY: messages to our own control; pointer arguments outlive the call.
    unsafe { SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

pub struct Preview {
    pub hwnd: HWND,
}

impl Preview {
    pub fn create(parent: HWND) -> Preview {
        let style = WINDOW_STYLE(WS_CHILD.0 | WS_CLIPSIBLINGS.0 | WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32);
        // SAFETY: child of our window; the class comes from msftedit.dll.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                MSFTEDIT_CLASS,
                w!(""),
                style,
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
        send(hwnd, EM_SETUNDOLIMIT, 0, 0);
        send(hwnd, EM_SETEVENTMASK, 0, 0);
        send(hwnd, EM_SETTARGETDEVICE, 0, 0); // wrap at the window
        // no grey grid lines around table cells: code blocks, quotes and
        // tables draw their own borders
        use windows::Win32::UI::Controls::RichEdit::{EM_SETEDITSTYLE, SES_HIDEGRIDLINES};
        send(hwnd, EM_SETEDITSTYLE, SES_HIDEGRIDLINES as usize, SES_HIDEGRIDLINES as isize);
        Preview { hwnd }
    }

    pub fn apply_theme(&self, theme: &Theme, pad: i32) {
        send(self.hwnd, EM_SETBKGNDCOLOR, 0, theme.colorref(theme.ground).0 as isize);
        // EC_LEFTMARGIN | EC_RIGHTMARGIN, both `pad`
        send(self.hwnd, EM_SETMARGINS, 0x3, ((pad as isize) << 16) | pad as isize);
        let name = if theme.dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
        // SAFETY: theme class for the scroll bar; unknown names are ignored.
        unsafe {
            let _ = SetWindowTheme(self.hwnd, name, PCWSTR::null());
        }
    }

    /// replaces the content, keeping the scroll position. `units` is the
    /// size of the markdown text: font fallback (emoji) only for texts that
    /// load fast with it (see editor::AUTOFONT_LIMIT).
    pub fn set_rtf(&self, rtf: &str, units: usize) {
        let mut pos = POINT::default();
        send(self.hwnd, EM_GETSCROLLPOS, 0, &mut pos as *mut _ as isize);
        let lang = send(self.hwnd, EM_GETLANGOPTIONS, 0, 0);
        let lang = if units <= crate::editor::AUTOFONT_LIMIT { lang | 0x2 } else { lang & !0x2 };
        send(self.hwnd, EM_SETLANGOPTIONS, 0, lang & !0x80);
        let bytes: Vec<u8> = rtf.bytes().chain(Some(0)).collect();
        let st = SETTEXTEX { flags: 0, codepage: 0 }; // rtf is 7-bit ascii
        send(self.hwnd, EM_SETTEXTEX, &st as *const _ as usize, bytes.as_ptr() as isize);
        send(self.hwnd, EM_SETSCROLLPOS, 0, &pos as *const _ as isize);
    }

    /// scrolls to the same share of the content as `fraction` (0..1) of
    /// the text: a simple, predictable sync with the editor.
    pub fn scroll_to_fraction(&self, fraction: f32) {
        let Some((max, page, _)) = scroll_info(self.hwnd) else { return };
        let room = (max - page).max(0);
        let y = (room as f32 * fraction.clamp(0.0, 1.0)).round() as i32;
        let p = POINT { x: 0, y };
        send(self.hwnd, EM_SETSCROLLPOS, 0, &p as *const _ as isize);
    }
}

/// (range max, page, position) of a window's vertical scroll bar.
pub fn scroll_info(h: HWND) -> Option<(i32, i32, i32)> {
    let mut si = SCROLLINFO { cbSize: size_of::<SCROLLINFO>() as u32, fMask: SIF_RANGE | SIF_PAGE | SIF_POS, ..Default::default() };
    // SAFETY: plain query on our own control.
    unsafe { GetScrollInfo(h, SB_VERT, &mut si).ok()? };
    Some((si.nMax - si.nMin + 1, si.nPage as i32, si.nPos - si.nMin))
}

/// where the editor is scrolled, as a share of what can be scrolled.
pub fn fraction_of(editor: HWND) -> f32 {
    match scroll_info(editor) {
        Some((max, page, pos)) if max > page => pos as f32 / (max - page) as f32,
        _ => 0.0,
    }
}
