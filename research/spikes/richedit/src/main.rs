//! ryad stage 1 spike: the system rich edit control in plain text mode.
//!
//!   richedit-spike [--class RICHEDIT50W|RichEditD2DPT] [--shot <file.bmp>] [--big]
//!
//! prints a markdown table of checks. nothing here touches user data; the
//! window sits off screen and only messages to our own controls are sent.

use std::ffi::c_void;
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDC, HGDIOBJ, ReleaseDC, SelectObject, UpdateWindow,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::Controls::RichEdit::*;
use windows::Win32::UI::Controls::{EM_SETMODIFY, EM_GETMODIFY, EM_REPLACESEL, EM_CANUNDO, EM_GETFIRSTVISIBLELINE, EM_LINEINDEX, EM_EMPTYUNDOBUFFER, EM_GETLINECOUNT, EM_SCROLLCARET, EM_UNDO};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, w};

fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    // SAFETY: messages to our own controls, pointers (when any) outlive the call.
    unsafe { SendMessageW(h, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn private_mib() -> f64 {
    let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: out-pointer to a local of the size passed.
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(), c.cb);
    }
    c.PrivateUsage as f64 / 1048576.0
}

fn pump() {
    let mut m = MSG::default();
    // SAFETY: plain message pump on this thread.
    unsafe {
        while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}

extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

struct Spike {
    parent: HWND,
    class: Vec<u16>,
}

impl Spike {
    fn edit(&self, limit: bool) -> HWND {
        let style = WINDOW_STYLE(
            WS_CHILD.0 | WS_VISIBLE.0 | WS_VSCROLL.0 | WS_HSCROLL.0 | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_AUTOHSCROLL as u32 | ES_NOHIDESEL as u32 | ES_WANTRETURN as u32,
        );
        // SAFETY: child of our own window; class registered by msftedit.dll.
        let h = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), PCWSTR(self.class.as_ptr()), w!(""), style, 0, 0, 800, 600, Some(self.parent), None, GetModuleHandleW(None).ok().map(|m| m.into()), None)
        }
        .expect("create rich edit");
        // plain text + multi-level undo + unicode; must be sent while empty.
        let r = send(h, EM_SETTEXTMODE, (TM_PLAINTEXT.0 | TM_MULTILEVELUNDO.0 | TM_MULTICODEPAGE.0) as usize, 0);
        assert_eq!(r, 0, "EM_SETTEXTMODE failed");
        if limit {
            send(h, EM_EXLIMITTEXT, 0, 0x7fff_fffe);
        }
        // which IMF_* bits to clear (default: autofont, size adjust, dual font)
        let clear: isize = std::env::var("SPIKE_CLEAR").ok().and_then(|v| isize::from_str_radix(&v, 16).ok()).unwrap_or(0x92);
        let lang = send(h, EM_GETLANGOPTIONS, 0, 0);
        send(h, EM_SETLANGOPTIONS, 0, lang & !clear);
        if std::env::var("SPIKE_CHARSET").is_ok() {
            // the default font as DEFAULT_CHARSET with a face that has cyrillic
            let mut cf = CHARFORMAT2W::default();
            cf.Base.cbSize = size_of::<CHARFORMAT2W>() as u32;
            cf.Base.dwMask = CFM_FACE | CFM_CHARSET;
            cf.Base.bCharSet = windows::Win32::Graphics::Gdi::FONT_CHARSET(1);
            let face = wide("Cascadia Mono");
            cf.Base.szFaceName[..face.len()].copy_from_slice(&face);
            send(h, EM_SETCHARFORMAT, SCF_ALL as usize, &cf as *const _ as isize);
        }
        h
    }

    fn set(&self, h: HWND, text: &str) {
        let t = wide(text);
        let st = SETTEXTEX { flags: 0, codepage: 1200 };
        send(h, EM_SETTEXTEX, &st as *const _ as usize, t.as_ptr() as isize);
    }
}

fn get(h: HWND, crlf: bool) -> String {
    let lenq = GETTEXTLENGTHEX { flags: GTL_PRECISE | GTL_NUMCHARS | if crlf { GTL_USECRLF } else { GTL_DEFAULT }, codepage: 1200 };
    let n = send(h, EM_GETTEXTLENGTHEX, &lenq as *const _ as usize, 0) as usize;
    let mut buf = vec![0u16; n + 1];
    let g = GETTEXTEX { cb: (buf.len() * 2) as u32, flags: if crlf { GT_USECRLF } else { GT_DEFAULT }, codepage: 1200, lpDefaultChar: windows::core::PCSTR::null(), lpUsedDefChar: std::ptr::null_mut() };
    let got = send(h, EM_GETTEXTEX, &g as *const _ as usize, buf.as_mut_ptr() as isize) as usize;
    String::from_utf16_lossy(&buf[..got.min(n)])
}

fn get_len(h: HWND) -> usize {
    let lenq = GETTEXTLENGTHEX { flags: GTL_PRECISE | GTL_NUMCHARS, codepage: 1200 };
    send(h, EM_GETTEXTLENGTHEX, &lenq as *const _ as usize, 0) as usize
}

fn sel(h: HWND) -> (i32, i32) {
    let mut c = CHARRANGE::default();
    send(h, EM_EXGETSEL, 0, &mut c as *mut _ as isize);
    (c.cpMin, c.cpMax)
}

fn set_sel(h: HWND, a: i32, b: i32) {
    let c = CHARRANGE { cpMin: a, cpMax: b };
    send(h, EM_EXSETSEL, 0, &c as *const _ as isize);
}

fn range_text(h: HWND, a: i32, b: i32) -> String {
    let mut buf = vec![0u16; (b - a) as usize + 2];
    let mut tr = TEXTRANGEW { chrg: CHARRANGE { cpMin: a, cpMax: b }, lpstrText: windows::core::PWSTR(buf.as_mut_ptr()) };
    let n = send(h, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn scroll(h: HWND) -> (i32, i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    send(h, EM_GETSCROLLPOS, 0, &mut p as *mut _ as isize);
    (p.x, p.y)
}

fn wrap(h: HWND, on: bool) {
    send(h, EM_SETTARGETDEVICE, 0, if on { 0 } else { 1 });
}

fn tom(h: HWND) -> Option<ITextDocument> {
    let mut p: *mut c_void = std::ptr::null_mut();
    if send(h, EM_GETOLEINTERFACE, 0, &mut p as *mut _ as isize) == 0 || p.is_null() {
        return None;
    }
    // SAFETY: EM_GETOLEINTERFACE hands out an AddRef'd IRichEditOle.
    let ole = unsafe { windows::core::IUnknown::from_raw(p) };
    ole.cast::<ITextDocument>().ok()
}

fn paragraph_of(doc: &ITextDocument, cp: i32) -> i32 {
    // SAFETY: plain TOM calls on our own control.
    unsafe { doc.Range(cp, cp).and_then(|r| r.GetIndex(tomParagraph.0)).unwrap_or(-1) }
}

fn esc(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\r' => o.push_str("\\r"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\u{0b}' => o.push_str("\\v"),
            '\u{0c}' => o.push_str("\\f"),
            '\0' => o.push_str("\\0"),
            '\u{2028}' => o.push_str("\\u2028"),
            '\u{2029}' => o.push_str("\\u2029"),
            '\u{85}' => o.push_str("\\u0085"),
            c if (c as u32) > 0x7f => o.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn row(name: &str, result: &str, ok: Option<bool>) {
    let mark = match ok {
        Some(true) => "✔",
        Some(false) => "✘",
        None => "·",
    };
    println!("| {name} | {} | {mark} |", result.replace('|', "\\|"));
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut class = "RICHEDIT50W".to_string();
    let mut shot = None;
    let mut big = false;
    let mut long = false;
    let mut long2 = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--class" => class = args.next().unwrap(),
            "--shot" => shot = args.next(),
            "--big" => big = true,
            "--long" => long = true,
            "--long2" => long2 = true,
            _ => panic!("unknown arg {a}"),
        }
    }
    // SAFETY: process setup.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let before_dll = private_mib();
    // SAFETY: system dll that registers the rich edit classes.
    unsafe { LoadLibraryW(w!("Msftedit.dll")).expect("msftedit") };
    let after_dll = private_mib();

    // SAFETY: own window class and an off-screen tool window as the parent.
    let parent = unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), lpszClassName: w!("ryad.spike"), hInstance: GetModuleHandleW(None).unwrap().into(), ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(WS_EX_TOOLWINDOW, w!("ryad.spike"), w!("ryad spike"), WS_POPUP | WS_VISIBLE, -4000, -4000, 820, 640, None, None, None, None).unwrap()
    };
    let s = Spike { parent, class: wide(&class) };

    println!("## class `{class}`\n");
    println!("msftedit.dll load: private {before_dll:.2} → {after_dll:.2} MiB\n");
    println!("| check | result | |\n|---|---|---|");

    // ---- load fidelity --------------------------------------------------
    let h = s.edit(true);
    let before_first = private_mib();
    let fixtures: &[(&str, &str)] = &[
        ("crlf", "один\r\nдва\r\n"),
        ("lf", "один\nдва\n"),
        ("lone cr", "один\rдва"),
        ("mixed", "a\r\nb\nc\rd"),
        ("no final newline", "a\r\nb"),
        ("vt / ff", "a\u{0b}b\u{0c}c"),
        ("u+2028 / u+2029 / u+0085", "a\u{2028}b\u{2029}c\u{85}d"),
        ("nul", "a\0b"),
        ("emoji + zwj", "😀 👩‍👩‍👧 🇺🇦"),
        ("combining", "й (и\u{306}) e\u{301}"),
        ("tab", "a\tb"),
        ("cr cr lf", "a\r\r\nb"),
    ];
    for (name, text) in fixtures {
        s.set(h, text);
        let raw = get(h, false);
        let crlf = get(h, true);
        let same = raw == *text || crlf == *text;
        row(&format!("fidelity: {name} `{}`", esc(text)), &format!("raw `{}` · crlf `{}`", esc(&raw), esc(&crlf)), Some(same));
    }

    // ---- position units --------------------------------------------------
    s.set(h, "a😀b\r\nc");
    let len = get_len(h);
    row("units: len of `a😀b\\r\\nc` (utf-16 = 6)", &format!("{len}"), None);
    set_sel(h, 3, 3);
    row("units: caret 3 = after emoji", &format!("sel {:?}, text 0..3 `{}`", sel(h), esc(&range_text(h, 0, 3))), Some(range_text(h, 0, 3) == "a😀"));
    set_sel(h, 2, 2);
    row("units: caret 2 = inside surrogate pair", &format!("snaps to {:?}", sel(h)), None);
    let c_pos = get(h, false).encode_utf16().position(|u| u == 'c' as u16).unwrap_or(99);
    row("units: cp of `c` (source utf-16 6, cr-only 5)", &format!("{c_pos}"), None);

    // ---- wrap toggle -----------------------------------------------------
    let long_line: String = (0..400).map(|i| format!("слово{i} ")).collect();
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(&format!("рядок {i} {}\r\n", if i % 10 == 0 { long_line.as_str() } else { "коротко" }));
    }
    s.set(h, &text);
    send(h, EM_SETMODIFY, 0, 0);
    for i in 0..30 {
        set_sel(h, (i * 37) as i32, (i * 37) as i32);
        let t = wide(&format!("[правка {i} 😀]"));
        send(h, EM_REPLACESEL, 1, t.as_ptr() as isize);
        send(h, EM_STOPGROUPTYPING, 0, 0);
    }
    set_sel(h, 5000, 5100);
    send(h, WM_VSCROLL, SB_PAGEDOWN.0 as usize, 0);
    send(h, WM_VSCROLL, SB_PAGEDOWN.0 as usize, 0);
    pump();
    let before = (get(h, false), sel(h), scroll(h), send(h, EM_GETMODIFY, 0, 0), send(h, EM_CANUNDO, 0, 0));
    let doc = tom(h);
    let first_vis_before = send(h, EM_GETFIRSTVISIBLELINE, 0, 0);
    let first_cp_before = send(h, EM_LINEINDEX, first_vis_before as usize, 0);
    let t = Instant::now();
    wrap(h, true);
    pump();
    let wrap_ms = t.elapsed().as_secs_f64() * 1000.0;
    let after = (get(h, false), sel(h), scroll(h), send(h, EM_GETMODIFY, 0, 0), send(h, EM_CANUNDO, 0, 0));
    row("wrap on: text identical", &format!("{}", before.0 == after.0), Some(before.0 == after.0));
    row("wrap on: selection", &format!("{:?} → {:?}", before.1, after.1), Some(before.1 == after.1));
    row("wrap on: modify flag", &format!("{} → {}", before.3, after.3), Some(before.3 == after.3));
    row("wrap on: can undo", &format!("{} → {}", before.4, after.4), Some(before.4 == after.4));
    row("wrap on: scroll pos (px)", &format!("{:?} → {:?}", before.2, after.2), None);
    let first_vis_after = send(h, EM_GETFIRSTVISIBLELINE, 0, 0);
    let first_cp_after = send(h, EM_LINEINDEX, first_vis_after as usize, 0);
    row("wrap on: first visible char", &format!("{first_cp_before} → {first_cp_after}"), None);
    row("wrap on: toggle time", &format!("{wrap_ms:.2} ms (200 lines, 20 long)"), None);
    wrap(h, false);
    pump();
    // undo depth survives two toggles
    let mut undos = 0;
    while send(h, EM_CANUNDO, 0, 0) != 0 && undos < 100 {
        send(h, EM_UNDO, 0, 0);
        undos += 1;
    }
    let restored = get(h, false) == text.replace("\r\n", "\r");
    row("undo depth after on/off toggle (30 edits)", &format!("{undos} undos, back to loaded text: {restored}, modify {}", send(h, EM_GETMODIFY, 0, 0)), Some(undos == 30 && restored));
    let mut redos = 0;
    while send(h, EM_CANREDO, 0, 0) != 0 && redos < 100 {
        send(h, EM_REDO, 0, 0);
        redos += 1;
    }
    row("redo depth", &format!("{redos}, text = before toggle: {}", get(h, false) == before.0), Some(redos == 30 && get(h, false) == before.0));

    // EM_SETTEXTEX with ST_KEEPUNDO | ST_SELECTION replaces the selection as one undo unit
    s.set(h, "a x a x a x");
    send(h, EM_EMPTYUNDOBUFFER, 0, 0);
    set_sel(h, 0, -1);
    let t2 = wide("b x b x b x");
    let st = SETTEXTEX { flags: ST_KEEPUNDO | ST_SELECTION, codepage: 1200 };
    send(h, EM_SETTEXTEX, &st as *const _ as usize, t2.as_ptr() as isize);
    let one = send(h, EM_CANUNDO, 0, 0);
    send(h, EM_UNDO, 0, 0);
    row("replace all via ST_KEEPUNDO|ST_SELECTION: one undo", &format!("can undo {one}, after 1 undo `{}`, can undo {}", get(h, false), send(h, EM_CANUNDO, 0, 0)), Some(get(h, false) == "a x a x a x"));

    // ukrainian through WM_CHAR (what the keyboard layout delivers)
    s.set(h, "");
    send(h, EM_EMPTYUNDOBUFFER, 0, 0);
    for c in "привіт, ґанок ї є".encode_utf16() {
        send(h, WM_CHAR, c as usize, 1);
    }
    let typed = get(h, false);
    row("WM_CHAR cyrillic", &format!("`{typed}`"), Some(typed == "привіт, ґанок ї є"));
    // emoji via two WM_CHAR surrogates
    for c in "😀".encode_utf16() {
        send(h, WM_CHAR, c as usize, 1);
    }
    row("WM_CHAR surrogate pair", &format!("ends with emoji: {}", get(h, false).ends_with('😀')), Some(get(h, false).ends_with('😀')));
    send(h, WM_CHAR, '\r' as usize, 1);
    row("enter inserts", &format!("`{}`", esc(&get(h, false))), None);

    // default limit
    let d = s.edit(false);
    let big100k: String = "x".repeat(100_000);
    s.set(d, &big100k);
    row("no EM_EXLIMITTEXT: set 100k chars", &format!("{} chars kept", get_len(d)), None);
    set_sel(d, 0, -1);
    let t = wide(&big100k);
    send(d, EM_REPLACESEL, 1, t.as_ptr() as isize);
    row("no EM_EXLIMITTEXT: paste 100k chars", &format!("{} chars kept", get_len(d)), None);
    // SAFETY: our own control.
    unsafe {
        let _ = DestroyWindow(d);
    }

    // ---- appearance -------------------------------------------------------
    send(h, EM_SETBKGNDCOLOR, 0, 0x1e1c1c);
    let mut cf = CHARFORMAT2W::default();
    cf.Base.cbSize = size_of::<CHARFORMAT2W>() as u32;
    cf.Base.dwMask = CFM_COLOR | CFM_FACE | CFM_SIZE;
    cf.Base.crTextColor = COLORREF(0xf2f2f2);
    cf.Base.yHeight = 11 * 20;
    let face = wide("Cascadia Mono");
    cf.Base.szFaceName[..face.len()].copy_from_slice(&face);
    send(h, EM_SETCHARFORMAT, SCF_ALL as usize, &cf as *const _ as isize);
    s.set(h, "ryad · чернетка\r\nукраїнська: ґ є ї і — «лапки»\r\nemoji: 😀 👩‍👩‍👧 🇺🇦 ❤️\r\ncombining: e\u{301} и\u{306}\r\n\tтаб\r\nвиділення ось тут ↓\r\nselected text here");
    set_sel(h, 95, 130);
    if let Some(path) = &shot {
        pump();
        // SAFETY: plain gdi into our own dib.
        unsafe {
            let _ = UpdateWindow(parent);
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: 40, biWidth: 820, biHeight: -640, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(mem), &bi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            let ok = PrintWindow(parent, mem, PRINT_WINDOW_FLAGS(2)).as_bool();
            let data = std::slice::from_raw_parts(bits as *const u8, 820 * 640 * 4).to_vec();
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            let mut f = Vec::new();
            let size = 54 + data.len() as u32;
            f.extend(b"BM");
            f.extend(size.to_le_bytes());
            f.extend([0u8; 4]);
            f.extend(54u32.to_le_bytes());
            f.extend(40u32.to_le_bytes());
            f.extend(820i32.to_le_bytes());
            f.extend((-640i32).to_le_bytes());
            f.extend(1u16.to_le_bytes());
            f.extend(32u16.to_le_bytes());
            f.extend([0u8; 24]);
            f.extend(&data);
            std::fs::write(path, f).unwrap();
            row("screenshot (PrintWindow)", &format!("ok {ok} → {path}"), None);
        }
    }
    println!("| first control, private | {before_first:.2} MiB at creation → {:.2} MiB now | · |", private_mib());
    drop(doc);
    // SAFETY: our own control.
    unsafe {
        let _ = DestroyWindow(h);
    }
    pump();

    // ---- per-tab undo memory ----------------------------------------------
    let base = private_mib();
    let one_mib: String = (0..16384).map(|i| format!("{i:08} абвгґдеєжзиіїйклмнопрстуфхцчшщьюя .....\r\n")).collect::<String>();
    let mut tabs = Vec::new();
    for _ in 0..5 {
        let e = s.edit(true);
        s.set(e, &one_mib);
        tabs.push(e);
    }
    pump();
    let loaded = private_mib();
    let chunk = wide(&"правка ".repeat(150));
    for e in &tabs {
        for i in 0..200 {
            set_sel(*e, i * 1000, i * 1000 + 10);
            send(*e, EM_REPLACESEL, 1, chunk.as_ptr() as isize);
            send(*e, EM_STOPGROUPTYPING, 0, 0);
        }
    }
    pump();
    let edited = private_mib();
    for e in tabs.drain(..) {
        // SAFETY: our own controls.
        unsafe {
            let _ = DestroyWindow(e);
        }
    }
    pump();
    let freed = private_mib();
    row(
        "5 tabs × 1 MiB text (utf-8 size; 16384 lines)",
        &format!("private {base:.1} → loaded {loaded:.1} (+{:.1}) → 200 undoable edits each {edited:.1} (+{:.1}) → destroyed {freed:.1}", loaded - base, edited - loaded),
        None,
    );

    if big {
        big_files(&s);
    } else if long {
        long_lines(&s);
    } else if long2 {
        long_variants(&s);
    }
    println!();
}

fn big_files(s: &Spike) {
    // many short lines: ~32 bytes per line → 1 MiB ≈ 32k lines, 50 MiB ≈ 1.6M lines
    for mib in [1usize, 10, 50] {
        let line = "0000000 log line with some text\r\n"; // 33 bytes ascii
        let n = mib * 1048576 / line.len();
        let text: String = (0..n).map(|i| format!("{i:07} log line with some text\r\n")).collect();
        case(s, &format!("{mib} MiB, {n} short lines"), &text, false);
    }
    long_lines(s);
}

fn long_variants(s: &Spike) {
    // what makes one long line slow? 1 MiB each.
    let mixed: String = "abcdefghij слово ".repeat(1048576 / 19);
    let ascii: String = "abcdefghij word ".repeat(1048576 / 16);
    let nospace: String = "x".repeat(1048576);
    for (name, text) in [("ascii with spaces", &ascii), ("one ascii run, no spaces", &nospace), ("latin + cyrillic", &mixed)] {
        let h = s.edit(true);
        let t = Instant::now();
        s.set(h, text);
        let a = t.elapsed().as_secs_f64() * 1000.0;
        // SAFETY: our own control.
        unsafe { let _ = DestroyWindow(h); }
        // hidden control
        let h = s.edit(true);
        // SAFETY: our own control.
        unsafe { let _ = ShowWindow(h, SW_HIDE); }
        let t = Instant::now();
        s.set(h, text);
        let b = t.elapsed().as_secs_f64() * 1000.0;
        // SAFETY: our own control.
        unsafe { let _ = DestroyWindow(h); }
        // no automatic font switching per script (IMF_AUTOFONT / IMF_DUALFONT off)
        let h = s.edit(true);
        let lang = send(h, EM_GETLANGOPTIONS, 0, 0);
        send(h, EM_SETLANGOPTIONS, 0, lang & !(0x2 | 0x80 | 0x1 | 0x10));
        println!("lang options were {lang:#x}");
        let t = Instant::now();
        s.set(h, text);
        let c = t.elapsed().as_secs_f64() * 1000.0;
        // SAFETY: our own control.
        unsafe { let _ = DestroyWindow(h); }
        // the same text split into 1000-char lines
        let lines: String = text.chars().collect::<Vec<_>>().chunks(1000).map(|c| c.iter().collect::<String>() + "\r").collect();
        let h = s.edit(true);
        let t = Instant::now();
        s.set(h, &lines);
        let d = t.elapsed().as_secs_f64() * 1000.0;
        // SAFETY: our own control.
        unsafe { let _ = DestroyWindow(h); }
        row(&format!("long line 1 MiB: {name}"), &format!("visible {a:.0} ms · hidden {b:.0} ms · no autofont {c:.0} ms · same text in 1000-char lines {d:.0} ms"), None);
    }
}

fn long_lines(s: &Spike) {
    // one line without breaks: no wrap first (grows faster than linear in
    // run 1), then with wrap turned on before the text is set.
    for kib in [64usize, 256, 1024] {
        let text: String = "abcdefghij слово ".repeat(kib * 1024 / 19);
        case(s, &format!("{kib} KiB, one line, no wrap"), &text, false);
    }
    for kib in [1024usize, 10240, 51200] {
        let text: String = "abcdefghij слово ".repeat(kib * 1024 / 19);
        case(s, &format!("{} MiB, one line, wrap before load", kib / 1024), &text, true);
    }
}

fn case(s: &Spike, name: &str, text: &str, wrap_first: bool) {
    let base = private_mib();
    let h = s.edit(true);
    if wrap_first {
        wrap(h, true);
    }
    let t = Instant::now();
    s.set(h, text);
    let load = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let lines = send(h, EM_GETLINECOUNT, 0, 0);
    let count = t.elapsed().as_secs_f64() * 1000.0;
    pump();
    let loaded = private_mib();
    let end = get_len(h) as i32;
    let t = Instant::now();
    wrap(h, true);
    let vlines = send(h, EM_GETLINECOUNT, 0, 0);
    let wrap_ms = t.elapsed().as_secs_f64() * 1000.0;
    let doc = tom(h);
    let t = Instant::now();
    let para = doc.as_ref().map(|d| paragraph_of(d, end)).unwrap_or(-1);
    let tom_end = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let para_mid = doc.as_ref().map(|d| paragraph_of(d, end / 2)).unwrap_or(-1);
    let tom_mid = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let vline = send(h, EM_EXLINEFROMCHAR, 0, end as isize);
    let vline_ms = t.elapsed().as_secs_f64() * 1000.0;
    set_sel(h, end, end);
    let t = Instant::now();
    send(h, EM_SCROLLCARET, 0, 0);
    pump();
    let scroll_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let back = get(h, false);
    let get_ms = t.elapsed().as_secs_f64() * 1000.0;
    let same = back.len() == text.replace("\r\n", "\r").len();
    let wrapped = private_mib();
    drop(doc);
    drop(back);
    // SAFETY: our own control.
    unsafe {
        let _ = DestroyWindow(h);
    }
    pump();
    let freed = private_mib();
    row(
        &format!("big: {name}"),
        &format!(
            "set {load:.0} ms · lines {lines} ({count:.0} ms) · wrap on {wrap_ms:.0} ms → {vlines} visual · tom paragraph(end) {para} in {tom_end:.1} ms, (mid) {para_mid} in {tom_mid:.1} ms · EM_EXLINEFROMCHAR(end) {vline} in {vline_ms:.1} ms · caret to end {scroll_ms:.0} ms · get text {get_ms:.0} ms ({same}) · private {base:.0} → {loaded:.0} → {wrapped:.0} → freed {freed:.0} MiB"
        ),
        None,
    );
}
