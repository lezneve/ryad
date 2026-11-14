//! the drawing kit for ryad's own surfaces (tab strip, status line, gutter,
//! find panel, banner): kadr's graphite theme, fonts with explicit fallbacks,
//! and small gdi helpers with double buffering.
//!
//! the palette and the font fallbacks come from kadr (src/ui.rs at 29db81d,
//! MIT, same authors). kadr draws its settings window with a software
//! direct2d target in a short-lived process because directwrite kept 5-8 mb
//! in the process after use (kadr docs/measurements.md). ryad's window is
//! open all the time, so it uses plain gdi: flat fills, hairlines and text.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FF_DONTCARE, FW_NORMAL,
    FillRect, GetTextExtentPoint32W, GetTextFaceW, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{PCWSTR, w};

use crate::wide;

#[derive(Clone, Copy, PartialEq)]
pub struct Theme {
    pub ground: [f32; 3],
    pub raised: [f32; 3],
    pub hover: [f32; 3],
    pub ink: [f32; 3],
    pub ink2: [f32; 3],
    pub ink3: [f32; 3],
    pub hair: [f32; 3],
    pub cap: [f32; 3],
    pub seg_on: [f32; 3],
    pub alert: [f32; 3],
    pub dark: bool,
}

pub fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0]
}

impl Theme {
    /// the windows app theme, light or dark. RYAD_THEME=light|dark overrides
    /// it for tests.
    pub fn current() -> Theme {
        if apps_use_light_theme() {
            Theme {
                ground: rgb(0xf5f5f6),
                raised: rgb(0xffffff),
                hover: rgb(0xebebed),
                ink: rgb(0x1d1d1f),
                ink2: rgb(0x5b5b60),
                ink3: rgb(0x6e6e73),
                // kadr's 10 % hairline, flattened onto the ground (gdi has no alpha)
                hair: rgb(0xdedee0),
                cap: rgb(0xf0f0f2),
                seg_on: rgb(0xffffff),
                alert: rgb(0xc4302b),
                dark: false,
            }
        } else {
            Theme {
                ground: rgb(0x1c1c1e),
                raised: rgb(0x242426),
                hover: rgb(0x2c2c2e),
                ink: rgb(0xf2f2f2),
                ink2: rgb(0xa1a1a6),
                ink3: rgb(0x98989d),
                hair: rgb(0x343436),
                cap: rgb(0x2c2c2e),
                seg_on: rgb(0x3a3a3c),
                alert: rgb(0xff7b72),
                dark: true,
            }
        }
    }

    pub fn colorref(&self, c: [f32; 3]) -> COLORREF {
        colorref(c)
    }
}

pub fn colorref(c: [f32; 3]) -> COLORREF {
    let b = |v: f32| (v * 255.0).round() as u32;
    COLORREF(b(c[0]) | (b(c[1]) << 8) | (b(c[2]) << 16))
}

/// windows high contrast: then ryad leaves colours to the system.
pub fn high_contrast() -> bool {
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut hc = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    // SAFETY: out-pointer to a local of the size given in cbSize.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some((&mut hc as *mut HIGHCONTRASTW).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    ok.is_ok() && (hc.dwFlags & HCF_HIGHCONTRASTON) == HCF_HIGHCONTRASTON
}

/// the high contrast palette from the system colours.
pub fn system_theme() -> Theme {
    use windows::Win32::Graphics::Gdi::{COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor};
    let c = |i| {
        // SAFETY: plain query.
        let v = unsafe { GetSysColor(i) };
        [(v & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, ((v >> 16) & 0xff) as f32 / 255.0]
    };
    let ground = c(COLOR_BTNFACE);
    let window = c(COLOR_WINDOW);
    let ink = c(COLOR_WINDOWTEXT);
    Theme {
        ground,
        raised: window,
        hover: c(COLOR_HIGHLIGHT),
        ink,
        ink2: ink,
        ink3: c(COLOR_GRAYTEXT),
        hair: ink,
        cap: window,
        seg_on: window,
        alert: ink,
        dark: (window[0] + window[1] + window[2]) < 1.5,
    }
}

fn apps_use_light_theme() -> bool {
    match std::env::var("RYAD_THEME").as_deref() {
        Ok("light") => return true,
        Ok("dark") => return false,
        _ => {}
    }
    let mut value: u32 = 1;
    let mut size = 4u32;
    // SAFETY: out-pointers to locals of the size passed alongside.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_err() || value != 0
}

/// relative luminance (wcag 2) of an srgb colour.
fn luminance(c: [f32; 3]) -> f32 {
    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

pub fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// the windows accent colour, picked from the accent palette so it reads on
/// `background`: the shade nearest the accent with contrast 4.5:1 or more,
/// lighter shades on a dark background, darker on a light one. the palette
/// (`Explorer\Accent\AccentPalette`, 8 rgba entries, lightest first, the
/// accent itself at 3) is what windows uses for its own accent shades.
pub fn accent(background: [f32; 3], dark: bool) -> [f32; 3] {
    let mut buf = [0u8; 32];
    let mut size = 32u32;
    // SAFETY: out-pointers to a local buffer of the size passed alongside.
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            windows::Win32::System::Registry::RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .is_ok()
        && size >= 28;
    let shade = |i: usize| [buf[i * 4] as f32 / 255.0, buf[i * 4 + 1] as f32 / 255.0, buf[i * 4 + 2] as f32 / 255.0];
    let fallback = if dark { rgb(0x60cdff) } else { rgb(0x005fb8) }; // windows 11 default blue shades
    if !ok {
        return fallback;
    }
    let order: [usize; 4] = if dark { [3, 2, 1, 0] } else { [3, 4, 5, 6] };
    order.iter().map(|&i| shade(i)).find(|&c| contrast(c, background) >= 4.5).unwrap_or(fallback)
}

pub fn system_dpi() -> u32 {
    // SAFETY: plain query.
    unsafe { GetDpiForSystem() }.max(96)
}

/// gdi fonts for one dpi. dropped (and deleted) when the dpi changes.
pub struct Fonts {
    pub ui: HFONT,
    pub small: HFONT,
    pub mono: HFONT,
    /// the editor's face (null-terminated): cascadia mono, else consolas
    pub mono_face: Vec<u16>,
    pub editor_px: i32,
    /// face names for the markdown preview's rtf
    pub ui_name: String,
    pub display_name: String,
    pub mono_name: String,
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: our own fonts, not selected into any dc between paints.
        unsafe {
            for f in [self.ui, self.small, self.mono] {
                let _ = DeleteObject(HGDIOBJ(f.0));
            }
        }
    }
}

fn font(face: &str, px: i32) -> HFONT {
    let f = wide(face);
    // SAFETY: plain font creation; the caller deletes it.
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            FF_DONTCARE.0 as u32,
            PCWSTR(f.as_ptr()),
        )
    }
}

/// the first face that gdi really gives us (it substitutes silently).
fn first_installed(hdc: HDC, faces: &[&str]) -> String {
    for face in faces {
        let f = font(face, 12);
        let mut buf = [0u16; 64];
        // SAFETY: the font is selected only for the query, then deleted.
        let got = unsafe {
            let old = SelectObject(hdc, HGDIOBJ(f.0));
            let n = GetTextFaceW(hdc, Some(&mut buf));
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(f.0));
            String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
        };
        if got.eq_ignore_ascii_case(face) {
            return face.to_string();
        }
    }
    faces[faces.len() - 1].to_string()
}

pub fn make_fonts(hdc: HDC, dpi: u32) -> Fonts {
    let px = |v: i32| v * dpi as i32 / 96;
    let ui = first_installed(hdc, &["Segoe UI Variable Text", "Segoe UI"]);
    let mono = first_installed(hdc, &["Cascadia Mono", "Consolas"]);
    let display = first_installed(hdc, &["Segoe UI Variable Display", "Segoe UI"]);
    Fonts {
        ui: font(&ui, px(13)),
        small: font(&ui, px(12)),
        mono: font(&mono, px(12)),
        mono_face: wide(&mono),
        editor_px: px(14),
        ui_name: ui,
        display_name: display,
        mono_name: mono,
    }
}

pub fn px(dpi: u32, v: i32) -> i32 {
    v * dpi as i32 / 96
}

/// a paint pass into an off-screen bitmap, copied to the window at the end:
/// no flicker on resize.
pub struct Paint {
    pub dc: HDC,
    pub rect: RECT,
    hwnd: HWND,
    ps: PAINTSTRUCT,
    window_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Paint {
    pub fn begin(hwnd: HWND) -> Paint {
        let mut ps = PAINTSTRUCT::default();
        let mut rect = RECT::default();
        // SAFETY: paired with EndPaint in drop; the bitmap and dc are ours.
        unsafe {
            let window_dc = BeginPaint(hwnd, &mut ps);
            let _ = GetClientRect(hwnd, &mut rect);
            let dc = CreateCompatibleDC(Some(window_dc));
            let bitmap = CreateCompatibleBitmap(window_dc, rect.right.max(1), rect.bottom.max(1));
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            SetBkMode(dc, TRANSPARENT);
            Paint { dc, rect, hwnd, ps, window_dc, bitmap, old }
        }
    }

    pub fn fill(&self, r: RECT, c: [f32; 3]) {
        fill(self.dc, r, c);
    }

    pub fn text(&self, s: &str, r: RECT, font: HFONT, c: [f32; 3], align: Align) {
        text(self.dc, s, r, font, c, align);
    }
}

impl Drop for Paint {
    fn drop(&mut self) {
        // SAFETY: copy the finished frame, then release what begin() made.
        unsafe {
            let _ = BitBlt(self.window_dc, 0, 0, self.rect.right, self.rect.bottom, Some(self.dc), 0, 0, SRCCOPY);
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
            let _ = EndPaint(self.hwnd, &self.ps);
        }
    }
}

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub fn fill(dc: HDC, r: RECT, c: [f32; 3]) {
    // SAFETY: a temporary brush, deleted right after use.
    unsafe {
        let b = CreateSolidBrush(colorref(c));
        FillRect(dc, &r, b);
        let _ = DeleteObject(HGDIOBJ(b.0));
    }
}

pub fn text(dc: HDC, s: &str, r: RECT, font: HFONT, c: [f32; 3], align: Align) {
    // DrawTextW with an empty string and DT_END_ELLIPSIS never returns
    // (seen on windows 11 26200): nothing to draw anyway.
    if s.is_empty() || r.right <= r.left {
        return;
    }
    let mut t: Vec<u16> = s.encode_utf16().collect();
    let mut r = r;
    let a = match align {
        Align::Left => DT_LEFT,
        Align::Center => DT_CENTER,
        Align::Right => DT_RIGHT,
    };
    // SAFETY: the font is selected for this call only.
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetTextColor(dc, colorref(c));
        SetBkMode(dc, TRANSPARENT);
        DrawTextW(dc, &mut t, &mut r, a | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(dc, old);
    }
}

pub fn text_width(dc: HDC, s: &str, font: HFONT) -> i32 {
    let t: Vec<u16> = s.encode_utf16().collect();
    let mut size = windows::Win32::Foundation::SIZE::default();
    // SAFETY: the font is selected for this call only.
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let _ = GetTextExtentPoint32W(dc, &t, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

pub fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT { left, top, right, bottom }
}

/// a push button drawn by ryad (NM_CUSTOMDRAW), still a real button for the
/// keyboard and screen readers. kadr's installer does the same.
#[allow(clippy::too_many_arguments)]
pub fn paint_button(dc: HDC, r: RECT, label: &str, font: HFONT, theme: &Theme, hot: bool, pressed: bool, focused: bool, on: bool) {
    let bg = if pressed || on {
        theme.seg_on
    } else if hot {
        theme.hover
    } else {
        theme.cap
    };
    fill(dc, r, theme.ground);
    let inner = RECT { left: r.left, top: r.top + 1, right: r.right, bottom: r.bottom - 1 };
    fill(dc, inner, bg);
    if focused {
        frame(dc, inner, theme.ink2);
    }
    text(dc, label, r, font, if on || hot || pressed { theme.ink } else { theme.ink2 }, Align::Center);
}

/// a 1 px outline.
pub fn frame(dc: HDC, r: RECT, c: [f32; 3]) {
    fill(dc, RECT { bottom: r.top + 1, ..r }, c);
    fill(dc, RECT { top: r.bottom - 1, ..r }, c);
    fill(dc, RECT { right: r.left + 1, ..r }, c);
    fill(dc, RECT { left: r.right - 1, ..r }, c);
}

/// dark title bar and caption colours in the theme's own ramp (documented
/// dwm attributes 20, 35, 36; windows 10 ignores 35 and 36).
pub fn apply_title_bar(hwnd: HWND, theme: &Theme) {
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
    let dark: i32 = theme.dark as i32;
    let caption = colorref(theme.ground).0;
    let textc = colorref(theme.ink).0;
    // SAFETY: documented attributes with values of the documented sizes.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), (&dark as *const i32).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), (&caption as *const u32).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), (&textc as *const u32).cast(), 4);
    }
}
