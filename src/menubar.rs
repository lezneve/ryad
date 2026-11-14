//! ryad's own menu bar and menus in the window's theme. the win32 menu bar
//! stays light in dark mode (no documented way to theme it), so the bar is a
//! painted row and every popup menu is owner-drawn: ground background, hover
//! tone, accelerator keys in quiet ink at the right.
//!
//! the menus themselves are ordinary win32 popup menus (TrackPopupMenuEx):
//! keyboard navigation, mnemonics (WM_MENUCHAR), WM_INITMENUPOPUP and
//! WM_COMMAND keep working. alt, f10 and alt+letter open the bar's menus;
//! left and right move between them, also with the mouse while one is open.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, ExcludeClipRect, HBRUSH, HDC, HGDIOBJ, MapWindowPoints};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_SELECTED};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui::{self, Align, Fonts, Paint, Theme, px, rect};

pub fn height(dpi: u32) -> i32 {
    px(dpi, 30)
}

pub struct Bar {
    pub menu: HMENU,
    titles: Vec<String>,
    rects: Vec<RECT>,
    pub hover: Option<usize>,
    pub open: Option<usize>,
    /// alt pressed alone (no other key before its release): opens the first menu
    pub alt_alone: bool,
}

thread_local! {
    /// item texts of owner-drawn menus, per menu, freed when a menu is redone
    static TEXTS: RefCell<HashMap<isize, Vec<Box<[u16]>>>> = RefCell::new(HashMap::new());
    static TRACK: RefCell<Track> = RefCell::new(Track::default());
    static BRUSH: RefCell<isize> = const { RefCell::new(0) };
}

/// the state of an open menu, for the message hook.
#[derive(Default)]
struct Track {
    top: isize,
    current: isize,
    on_popup_item: bool,
    count: usize,
    index: usize,
    /// set by the hook: open this menu next (left/right, mouse over the bar)
    switch_to: Option<usize>,
    rects: Vec<RECT>,
}

impl Bar {
    pub fn new(menu: HMENU) -> Bar {
        let mut titles = Vec::new();
        // SAFETY: reading item texts of our own menu.
        unsafe {
            for i in 0..GetMenuItemCount(Some(menu)).max(0) {
                let mut buf = [0u16; 64];
                let n = GetMenuStringW(menu, i as u32, Some(&mut buf), MF_BYPOSITION);
                titles.push(String::from_utf16_lossy(&buf[..n.max(0) as usize]));
            }
        }
        Bar { menu, titles, rects: Vec::new(), hover: None, open: None, alt_alone: false }
    }

    pub fn layout(&mut self, dc: HDC, fonts: &Fonts, dpi: u32) {
        let mut x = px(dpi, 6);
        let h = height(dpi);
        self.rects.clear();
        for t in &self.titles {
            let w = ui::text_width(dc, &t.replace('&', ""), fonts.ui) + px(dpi, 20);
            self.rects.push(rect(x, px(dpi, 3), x + w, h - px(dpi, 3)));
            x += w;
        }
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.rects.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
    }

    /// the menu whose mnemonic is `ch` (alt+f → file).
    pub fn by_letter(&self, ch: char) -> Option<usize> {
        let ch = ch.to_ascii_lowercase();
        self.titles.iter().position(|t| t.find('&').and_then(|i| t[i + 1..].chars().next()).is_some_and(|c| c.to_ascii_lowercase() == ch))
    }

    pub fn paint(&self, p: &Paint, theme: &Theme, fonts: &Fonts) {
        p.fill(p.rect, theme.ground);
        for (i, (r, t)) in self.rects.iter().zip(&self.titles).enumerate() {
            let active = self.open == Some(i);
            if active || self.hover == Some(i) {
                p.fill(*r, if active { theme.seg_on } else { theme.hover });
            }
            p.text(&t.replace('&', ""), *r, fonts.ui, if active || self.hover == Some(i) { theme.ink } else { theme.ink2 }, Align::Center);
        }
    }

    /// opens menu `i` under its title and follows left/right to the next
    /// ones until a menu closes without switching. `owner` gets the
    /// WM_INITMENUPOPUP and WM_COMMAND messages as with a normal menu bar.
    pub fn track(&mut self, bar_hwnd: HWND, owner: HWND, mut i: usize, keyboard: bool) {
        loop {
            let Some(r) = self.rects.get(i).copied() else { return };
            // SAFETY: our own menus; the hook lives only while the menu is open.
            let sub = unsafe { GetSubMenu(self.menu, i as i32) };
            if sub.is_invalid() {
                return;
            }
            let mut pts = [POINT { x: r.left, y: r.bottom }, POINT { x: r.right, y: r.top }];
            // SAFETY: plain coordinate mapping.
            unsafe {
                MapWindowPoints(Some(bar_hwnd), None, &mut pts);
            }
            self.open = Some(i);
            // SAFETY: repaint of our own bar.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(bar_hwnd), None, false);
                let _ = windows::Win32::Graphics::Gdi::UpdateWindow(bar_hwnd);
            }
            let mut screen_rects = Vec::new();
            for rr in &self.rects {
                let mut q = [POINT { x: rr.left, y: rr.top }, POINT { x: rr.right, y: rr.bottom }];
                // SAFETY: plain coordinate mapping.
                unsafe {
                    MapWindowPoints(Some(bar_hwnd), None, &mut q);
                }
                screen_rects.push(rect(q[0].x, q[0].y, q[1].x, q[1].y));
            }
            TRACK.with(|t| {
                *t.borrow_mut() = Track {
                    top: sub.0 as isize,
                    current: sub.0 as isize,
                    on_popup_item: false,
                    count: self.titles.len(),
                    index: i,
                    switch_to: None,
                    rects: screen_rects,
                }
            });
            // SAFETY: a thread-local message filter hook for the menu loop,
            // removed right after TrackPopupMenuEx returns.
            unsafe {
                let hook = SetWindowsHookExW(WH_MSGFILTER, Some(filter), None, windows::Win32::System::Threading::GetCurrentThreadId()).ok();
                if keyboard {
                    // select the first item, as a keyboard-opened menu bar does
                    let _ = PostMessageW(Some(owner), WM_KEYDOWN, WPARAM(VK_DOWN_CODE), LPARAM(0));
                }
                let exclude = TPMPARAMS { cbSize: size_of::<TPMPARAMS>() as u32, rcExclude: rect(pts[0].x, pts[1].y, pts[1].x, pts[0].y) };
                let _ = TrackPopupMenuEx(sub, (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_VERTICAL).0, pts[0].x, pts[0].y, owner, Some(&exclude));
                if let Some(h) = hook {
                    let _ = UnhookWindowsHookEx(h);
                }
            }
            self.open = None;
            // SAFETY: repaint of our own bar.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(bar_hwnd), None, false);
            }
            match TRACK.with(|t| t.borrow_mut().switch_to.take()) {
                Some(next) => i = next,
                None => return,
            }
        }
    }
}

const VK_DOWN_CODE: usize = 0x28;

/// WM_MENUSELECT from the owner: which menu and item are highlighted.
pub fn on_menu_select(wparam: WPARAM, lparam: LPARAM) {
    let flags = ((wparam.0 >> 16) & 0xffff) as u32;
    TRACK.with(|t| {
        let mut t = t.borrow_mut();
        t.current = lparam.0;
        t.on_popup_item = flags & MF_POPUP.0 != 0;
    });
}

unsafe extern "system" fn filter(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == MSGF_MENU as i32 {
        // SAFETY: for WH_MSGFILTER, lparam points to the MSG being processed.
        let msg = unsafe { &*(lparam.0 as *const MSG) };
        let switch = TRACK.with(|t| {
            let t = t.borrow();
            match msg.message {
                WM_KEYDOWN if msg.wParam.0 == 0x27 && t.current == t.top && !t.on_popup_item => Some((t.index + 1) % t.count.max(1)),
                WM_KEYDOWN if msg.wParam.0 == 0x25 && t.current == t.top => Some((t.index + t.count.max(1) - 1) % t.count.max(1)),
                WM_MOUSEMOVE => {
                    let mut pt = POINT::default();
                    // SAFETY: plain query.
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                    }
                    t.rects.iter().position(|r| pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom).filter(|&k| k != t.index)
                }
                _ => None,
            }
        });
        if let Some(next) = switch {
            TRACK.with(|t| t.borrow_mut().switch_to = Some(next));
            // SAFETY: closes the open menu; track() then opens the next one.
            unsafe {
                let _ = EndMenu();
            }
            return LRESULT(1);
        }
    }
    // SAFETY: pass everything else on.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

// ---- owner-drawn popup menus ---------------------------------------------------------

/// makes every item of `menu` owner-drawn (keeping its text for drawing and
/// mnemonics) and gives the menu the theme's background. called for each
/// popup in WM_INITMENUPOPUP, after its items are up to date.
pub fn theme_menu(menu: HMENU, theme: &Theme) {
    // the background brush, replaced when the theme changes
    BRUSH.with(|b| {
        let mut b = b.borrow_mut();
        // SAFETY: our own brush; the old one is deleted once replaced.
        unsafe {
            if *b != 0 {
                let _ = DeleteObject(HGDIOBJ(*b as *mut _));
            }
            *b = CreateSolidBrush(ui::colorref(theme.raised)).0 as isize;
        }
    });
    let brush = BRUSH.with(|b| HBRUSH(*b.borrow() as *mut _));
    let mut texts = Vec::new();
    // SAFETY: reading and changing items of our own menu.
    unsafe {
        let mi = MENUINFO { cbSize: size_of::<MENUINFO>() as u32, fMask: MIM_BACKGROUND, hbrBack: brush, ..Default::default() };
        let _ = SetMenuInfo(menu, &mi);
        for pos in 0..GetMenuItemCount(Some(menu)).max(0) as u32 {
            let mut buf = [0u16; 256];
            let mut info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STRING | MIIM_DATA,
                dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
                cch: buf.len() as u32,
                ..Default::default()
            };
            if GetMenuItemInfoW(menu, pos, true, &mut info).is_err() {
                continue;
            }
            if info.fType.contains(MFT_OWNERDRAW) {
                // already done: its text pointer lives in the old list, keep it
                if info.dwItemData != 0 {
                    let len = (0..).take_while(|&k| *(info.dwItemData as *const u16).add(k) != 0).count();
                    let s = std::slice::from_raw_parts(info.dwItemData as *const u16, len + 1).to_vec().into_boxed_slice();
                    let new =
                        MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_DATA, dwItemData: s.as_ptr() as usize, ..Default::default() };
                    let _ = SetMenuItemInfoW(menu, pos, true, &new);
                    texts.push(s);
                }
                continue;
            }
            let separator = info.fType.contains(MFT_SEPARATOR);
            let text: Box<[u16]> = if separator { vec![0u16].into_boxed_slice() } else { buf[..info.cch as usize].iter().copied().chain(Some(0)).collect() };
            let new = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_DATA,
                fType: info.fType | MFT_OWNERDRAW,
                dwItemData: text.as_ptr() as usize,
                ..Default::default()
            };
            let _ = SetMenuItemInfoW(menu, pos, true, &new);
            texts.push(text);
        }
    }
    TEXTS.with(|t| {
        t.borrow_mut().insert(menu.0 as isize, texts);
    });
}

fn item_text(data: usize) -> String {
    if data == 0 {
        return String::new();
    }
    // SAFETY: data points at a nul-terminated string kept in TEXTS while the
    // menu exists.
    unsafe {
        let p = data as *const u16;
        let len = (0..).take_while(|&k| *p.add(k) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

pub fn measure(mis: &mut MEASUREITEMSTRUCT, dc: HDC, fonts: &Fonts, dpi: u32) {
    let text = item_text(mis.itemData);
    if text.is_empty() {
        mis.itemWidth = px(dpi, 40) as u32;
        mis.itemHeight = px(dpi, 9) as u32;
        return;
    }
    let (label, keys) = text.split_once('\t').unwrap_or((&text, ""));
    let w = ui::text_width(dc, &label.replace('&', ""), fonts.ui) + ui::text_width(dc, keys, fonts.small);
    // windows adds room for the check mark column on its own
    mis.itemWidth = (w + px(dpi, 64)) as u32;
    mis.itemHeight = px(dpi, 30) as u32;
}

pub fn draw(dis: &DRAWITEMSTRUCT, theme: &Theme, fonts: &Fonts, dpi: u32) {
    let checked = (dis.itemState.0 & windows::Win32::UI::Controls::ODS_CHECKED.0) != 0;
    let r = dis.rcItem;
    let dc = dis.hDC;
    let text = item_text(dis.itemData);
    let selected = (dis.itemState.0 & ODS_SELECTED.0) != 0;
    let disabled = (dis.itemState.0 & ODS_DISABLED.0) != 0;
    ui::fill(dc, r, theme.raised);
    if text.is_empty() {
        let y = (r.top + r.bottom) / 2;
        ui::fill(dc, rect(r.left + px(dpi, 10), y, r.right - px(dpi, 10), y + 1), theme.hair);
        return;
    }
    if selected && !disabled {
        ui::fill(dc, rect(r.left + px(dpi, 4), r.top + px(dpi, 1), r.right - px(dpi, 4), r.bottom - px(dpi, 1)), theme.hover);
    }
    let ink = if disabled { theme.ink3 } else { theme.ink };
    let (label, keys) = text.split_once('\t').unwrap_or((&text, ""));
    let check_w = px(dpi, 28);
    if checked {
        ui::text(dc, "✓", rect(r.left, r.top, r.left + check_w, r.bottom), fonts.ui, ink, Align::Center);
    }
    ui::text(dc, &label.replace('&', ""), rect(r.left + check_w, r.top, r.right - px(dpi, 24), r.bottom), fonts.ui, ink, Align::Left);
    let arrow_w = px(dpi, 20);
    if !keys.is_empty() {
        ui::text(dc, keys, rect(r.left, r.top, r.right - arrow_w, r.bottom), fonts.small, theme.ink3, Align::Right);
    }
    // a submenu: our own arrow, and the system's (black) one clipped away
    // SAFETY: plain query and clip change on the dc of this draw call.
    unsafe {
        let is_popup = IsMenu(HMENU(dis.itemID as usize as *mut _)).as_bool() && dis.itemID as usize > 0xffff;
        if is_popup {
            ui::text(dc, "›", rect(r.right - arrow_w, r.top, r.right - px(dpi, 6), r.bottom), fonts.ui, theme.ink2, Align::Center);
            ExcludeClipRect(dc, r.right - arrow_w, r.top, r.right, r.bottom);
        }
    }
}

/// WM_MENUCHAR: owner-drawn items have no text for windows to search, so
/// the mnemonic is found here. returns the result for the message.
pub fn menu_char(ch: u16, menu: HMENU) -> LRESULT {
    let want = char::from_u32(ch as u32).map(|c| c.to_lowercase().next().unwrap_or(c));
    // SAFETY: reading items of a menu that is open.
    unsafe {
        for pos in 0..GetMenuItemCount(Some(menu)).max(0) as u32 {
            let mut info = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_DATA, ..Default::default() };
            if GetMenuItemInfoW(menu, pos, true, &mut info).is_err() {
                continue;
            }
            let text = item_text(info.dwItemData);
            let m = text.find('&').and_then(|i| text[i + 1..].chars().next()).map(|c| c.to_lowercase().next().unwrap_or(c));
            if m.is_some() && m == want {
                return LRESULT(((MNC_EXECUTE as isize) << 16) | pos as isize);
            }
        }
    }
    LRESULT(0)
}
