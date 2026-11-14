//! the tab strip: layout, painting and hit testing. compact horizontal tabs,
//! a `+`, and a list button when the tabs do not fit. the active tab shares
//! the editor's surface and carries an ink bar; a dot marks changes that are
//! not in the user's file yet; × appears on the active and hovered tab.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use crate::ui::{self, Align, Fonts, Paint, Theme, px, rect};

pub struct TabInfo {
    pub title: String,
    pub dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    New,
    List,
}

#[derive(Default)]
pub struct Strip {
    /// horizontal scroll of the tabs, in pixels
    pub scroll: i32,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    tabs: Vec<RECT>,
    new_button: RECT,
    list_button: Option<RECT>,
    area: (i32, i32),
}

pub fn height(dpi: u32) -> i32 {
    px(dpi, 36)
}

impl Strip {
    pub fn layout(&mut self, dc: HDC, fonts: &Fonts, dpi: u32, width: i32, tabs: &[TabInfo], active: usize) {
        let p = |v| px(dpi, v);
        let h = height(dpi);
        let (top, bottom) = (p(6), h);
        let mut widths = Vec::with_capacity(tabs.len());
        for t in tabs {
            let w = ui::text_width(dc, &t.title, fonts.ui) + p(12) + p(30);
            widths.push(w.clamp(p(88), p(220)));
        }
        let total: i32 = widths.iter().sum::<i32>() + p(2) * tabs.len() as i32;
        let button = p(32);
        let left = p(6);
        let overflow = left + total + button > width;
        let right_edge = if overflow { width - button * 2 } else { width - button };
        self.area = (left, right_edge.max(left + p(40)));
        self.list_button = overflow.then(|| rect(width - button, top, width, bottom));
        // keep the active tab in view
        let mut x = 0;
        let mut starts = Vec::with_capacity(tabs.len());
        for w in &widths {
            starts.push(x);
            x += w + p(2);
        }
        let visible = self.area.1 - self.area.0;
        if !overflow {
            self.scroll = 0;
        } else if let (Some(&s), Some(&w)) = (starts.get(active), widths.get(active)) {
            if s < self.scroll {
                self.scroll = s;
            } else if s + w > self.scroll + visible {
                self.scroll = s + w - visible;
            }
            self.scroll = self.scroll.clamp(0, (total - visible).max(0));
        }
        self.tabs = starts.iter().zip(&widths).map(|(&s, &w)| rect(left + s - self.scroll, top, left + s + w - self.scroll, bottom)).collect();
        let after = self.tabs.last().map(|r| r.right + p(2)).unwrap_or(left);
        self.new_button = if overflow { rect(width - button * 2, top, width - button, bottom) } else { rect(after, top, after + button, bottom) };
    }

    fn close_rect(&self, dpi: u32, r: RECT) -> RECT {
        let s = px(dpi, 20);
        let cy = (r.top + r.bottom) / 2;
        rect(r.right - px(dpi, 6) - s, cy - s / 2, r.right - px(dpi, 6), cy + s / 2)
    }

    pub fn hit(&self, dpi: u32, x: i32, y: i32) -> Option<Hit> {
        let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if let Some(l) = &self.list_button
            && inside(l)
        {
            return Some(Hit::List);
        }
        if inside(&self.new_button) {
            return Some(Hit::New);
        }
        if x < self.area.0 || x >= self.area.1 {
            return None;
        }
        for (i, r) in self.tabs.iter().enumerate() {
            if inside(r) {
                return Some(if inside(&self.close_rect(dpi, *r)) { Hit::Close(i) } else { Hit::Tab(i) });
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(&self, p: &Paint, theme: &Theme, fonts: &Fonts, dpi: u32, tabs: &[TabInfo], active: usize, window_active: bool) {
        let s = |v| px(dpi, v);
        p.fill(p.rect, theme.ground);
        // hairline under the strip, interrupted by the active tab
        p.fill(rect(0, p.rect.bottom - 1, p.rect.right, p.rect.bottom), theme.hair);
        let clip = self.area;
        for (i, (r, t)) in self.tabs.iter().zip(tabs).enumerate() {
            if r.right <= clip.0 || r.left >= clip.1 {
                continue;
            }
            let r = rect(r.left.max(clip.0), r.top, r.right.min(clip.1), r.bottom);
            let is_active = i == active;
            let hot = matches!(self.hover, Some(Hit::Tab(j)) | Some(Hit::Close(j)) if j == i);
            let bg = if is_active {
                theme.raised
            } else if hot {
                theme.hover
            } else {
                theme.ground
            };
            p.fill(r, bg);
            if is_active {
                // the ink bar: strong while ryad is the active window
                p.fill(rect(r.left, r.top, r.right, r.top + s(2)), if window_active { theme.ink } else { theme.ink3 });
            } else if i + 1 != active && !hot {
                // a quiet divider between inactive tabs
                p.fill(rect(r.right + s(1) - 1, r.top + s(8), r.right + s(1), r.bottom - s(8)), theme.hair);
            }
            let close = self.close_rect(dpi, r);
            let reserve = if t.dirty && (is_active || hot) { s(16) } else { s(2) };
            let text_r = rect(r.left + s(12), r.top, close.left - reserve, r.bottom);
            p.text(&t.title, text_r, fonts.ui, if is_active { theme.ink } else { theme.ink2 }, Align::Left);
            let close_hot = self.hover == Some(Hit::Close(i));
            if close_hot {
                p.fill(close, if self.pressed == Some(Hit::Close(i)) { theme.seg_on } else { theme.cap });
            }
            if is_active || hot {
                p.text("×", close, fonts.ui, if close_hot { theme.ink } else { theme.ink2 }, Align::Center);
            } else if t.dirty {
                p.text("●", close, fonts.small, theme.ink2, Align::Center);
            }
            if t.dirty && (is_active || hot) && !close_hot {
                // keep the dot visible next to the ×
                let dot = rect(close.left - s(14), close.top, close.left, close.bottom);
                p.text("●", dot, fonts.small, theme.ink2, Align::Center);
            }
        }
        for (hit, r) in [(Hit::New, Some(self.new_button)), (Hit::List, self.list_button)] {
            let Some(r) = r else { continue };
            let inner = rect(r.left + s(4), r.top + s(3), r.right - s(4), r.bottom - s(5));
            if self.hover == Some(hit) {
                p.fill(inner, if self.pressed == Some(hit) { theme.seg_on } else { theme.hover });
            }
            let (cx, cy) = ((inner.left + inner.right) / 2, (inner.top + inner.bottom) / 2);
            let w = s(1).max(1);
            if hit == Hit::New {
                // a plus from two bars, on whole pixels
                let a = s(5);
                p.fill(rect(cx - a, cy - w / 2, cx + a + 1, cy - w / 2 + w), theme.ink2);
                p.fill(rect(cx - w / 2, cy - a, cx - w / 2 + w, cy + a + 1), theme.ink2);
            } else {
                // a chevron from stepped bars
                let a = s(4);
                for k in 0..=a {
                    let y = cy - a / 2 + k;
                    p.fill(rect(cx - a + k, y, cx - a + k + w, y + w), theme.ink2);
                    p.fill(rect(cx + a - k - w + 1, y, cx + a - k + 1, y + w), theme.ink2);
                }
            }
        }
    }
}
