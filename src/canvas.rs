//! a software direct2d canvas for the installer window, ported from kadr
//! (src/ui.rs at 29db81d, MIT, same authors): rounded frames, switches and
//! pill buttons drawn with antialiasing, text through directwrite.
//!
//! only the installer uses it. it runs in its own short-lived process, so the
//! 5-8 mb directwrite keeps after first use (kadr docs/measurements.md) never
//! reach the editor's process, which draws with gdi (ui.rs).

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows::core::{PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::{trace, wide};

pub struct Fonts {
    pub body: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    /// body size that wraps, for paragraphs.
    pub para: IDWriteTextFormat,
    /// small size that wraps, for notes under a row.
    pub small_para: IDWriteTextFormat,
}

pub fn make_fonts(dwrite: &IDWriteFactory, dpi: u32) -> Result<Fonts> {
    let px = |v: f32| v * dpi as f32 / 96.0;
    let make = |family: PCWSTR, weight: DWRITE_FONT_WEIGHT, size: f32| -> Result<IDWriteTextFormat> {
        // SAFETY: plain format creation.
        unsafe {
            let f = dwrite.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(f)
        }
    };
    // windows 11 ships segoe ui variable and cascadia mono; windows 10 has
    // neither by default. without an explicit choice directwrite would fall
    // back silently to some unrelated face.
    let ui = installed(dwrite, &["Segoe UI Variable Text", "Segoe UI"]);
    let display = installed(dwrite, &["Segoe UI Variable Display", "Segoe UI"]);
    let para = make(PCWSTR(ui.as_ptr()), DWRITE_FONT_WEIGHT_NORMAL, px(13.0))?;
    // SAFETY: plain setters on our own format.
    unsafe {
        para.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        para.SetParagraphAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
    }
    Ok(Fonts {
        body: make(PCWSTR(ui.as_ptr()), DWRITE_FONT_WEIGHT_NORMAL, px(13.0))?,
        title: make(PCWSTR(display.as_ptr()), DWRITE_FONT_WEIGHT_MEDIUM, px(20.0))?,
        para,
        small_para: {
            let f = make(PCWSTR(ui.as_ptr()), DWRITE_FONT_WEIGHT_NORMAL, px(12.0))?;
            // SAFETY: plain setters on our own format.
            unsafe {
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                f.SetParagraphAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            }
            f
        },
    })
}

/// the first of `names` installed on this system (null-terminated utf-16);
/// the last name when none is found, which windows always has.
fn installed(dwrite: &IDWriteFactory, names: &[&str]) -> Vec<u16> {
    // SAFETY: out-pointers to locals; the collection is released on drop.
    unsafe {
        let mut collection = None;
        if dwrite.GetSystemFontCollection(&mut collection, false).is_ok()
            && let Some(collection) = collection
        {
            for name in names {
                let w = wide(name);
                let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
                if collection.FindFamilyName(PCWSTR(w.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool() {
                    trace!("ui: font {name}");
                    return w;
                }
            }
        }
    }
    wide(names[names.len() - 1])
}

/// a software direct2d target that paints into whatever dc it is given, in
/// physical pixels (target dpi 96).
pub struct Canvas {
    pub dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
}

impl Canvas {
    pub fn new() -> Result<Canvas> {
        // SAFETY: plain factory and resource creation.
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let target = d2d.CreateDCRenderTarget(&props)?;
            let brush = target.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?;
            Ok(Canvas { dwrite, target, brush })
        }
    }

    pub fn measure(&self, text: &str, format: &IDWriteTextFormat) -> f32 {
        self.measure_box(text, format, 10000.0).0
    }

    /// (width, height) of `text` laid out within `max_width`.
    pub fn measure_box(&self, text: &str, format: &IDWriteTextFormat, max_width: f32) -> (f32, f32) {
        let t: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain layout creation and metric query.
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&t, format, max_width, 10000.0) else { return (0.0, 0.0) };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut m);
            (m.widthIncludingTrailingWhitespace, m.height)
        }
    }

    /// draws into `hdc`, whose top-left sits at `origin` in the window's
    /// client coordinates; `f` draws in client coordinates.
    pub fn paint(&self, hdc: HDC, size: (i32, i32), origin: (i32, i32), f: impl FnOnce(&Painter)) {
        let bound = RECT { left: 0, top: 0, right: size.0, bottom: size.1 };
        // SAFETY: the dc is valid for the duration of this paint message.
        unsafe {
            if self.target.BindDC(hdc, &bound).is_err() {
                return;
            }
            self.target.BeginDraw();
            self.target.SetTransform(&Matrix3x2::translation(-origin.0 as f32, -origin.1 as f32));
            f(&Painter { c: self });
            let _ = self.target.EndDraw(None, None);
        }
    }
}

pub struct Painter<'a> {
    c: &'a Canvas,
}

pub fn frect(r: RECT) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left as f32, top: r.top as f32, right: r.right as f32, bottom: r.bottom as f32 }
}

impl Painter<'_> {
    fn color(&self, c: [f32; 4]) -> &ID2D1SolidColorBrush {
        // SAFETY: setting a colour on our own brush.
        unsafe { self.c.brush.SetColor(&D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] }) };
        &self.c.brush
    }
    pub fn fill(&self, r: RECT, c: [f32; 3]) {
        self.fill_f(frect(r), [c[0], c[1], c[2], 1.0]);
    }
    pub fn fill_f(&self, r: D2D_RECT_F, c: [f32; 4]) {
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe { self.c.target.FillRectangle(&r, self.color(c)) };
    }
    pub fn fill_round(&self, r: RECT, radius: f32, c: [f32; 3]) {
        let rr = D2D1_ROUNDED_RECT { rect: frect(r), radiusX: radius, radiusY: radius };
        // SAFETY: as above.
        unsafe { self.c.target.FillRoundedRectangle(&rr, self.color([c[0], c[1], c[2], 1.0])) };
    }
    pub fn stroke_round(&self, r: RECT, radius: f32, c: [f32; 4]) {
        let f = frect(r);
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: f.left + 0.5, top: f.top + 0.5, right: f.right - 0.5, bottom: f.bottom - 0.5 },
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as above.
        unsafe { self.c.target.DrawRoundedRectangle(&rr, self.color(c), 1.0, None) };
    }
    pub fn line(&self, a: (f32, f32), b: (f32, f32), c: [f32; 4]) {
        self.line_w(a, b, c, 1.0);
    }
    pub fn line_w(&self, a: (f32, f32), b: (f32, f32), c: [f32; 4], width: f32) {
        // SAFETY: as above.
        unsafe { self.c.target.DrawLine(Vector2 { X: a.0, Y: a.1 }, Vector2 { X: b.0, Y: b.1 }, self.color(c), width, None) };
    }
    pub fn clip(&self, r: RECT, f: impl FnOnce()) {
        // SAFETY: push/pop are balanced.
        unsafe {
            self.c.target.PushAxisAlignedClip(&frect(r), D2D1_ANTIALIAS_MODE_ALIASED);
            f();
            self.c.target.PopAxisAlignedClip();
        }
    }
    fn draw_text(&self, text: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, c: [f32; 3]) {
        let t: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: as above.
        unsafe {
            self.c.target.DrawText(&t, format, &r, self.color([c[0], c[1], c[2], 1.0]), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }
    pub fn text(&self, text: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, c: [f32; 3]) {
        self.draw_text(text, format, r, c);
    }
    pub fn text_center(&self, text: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, c: [f32; 3]) {
        let w = self.c.measure(text, format);
        let left = r.left + ((r.right - r.left) - w) / 2.0;
        self.draw_text(text, format, D2D_RECT_F { left, right: left + w + 1.0, ..r }, c);
    }
}
