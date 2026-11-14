//! settings.ini in the data folder: only what ryad needs to reopen the
//! window as it was. separate from the session (docs/architecture.md).

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Settings {
    /// window rect in physical pixels (left, top, right, bottom), restored
    /// size even when maximized
    pub window: Option<[i32; 4]>,
    pub maximized: bool,
    /// share of the width the text takes when the preview is shown
    pub split: Option<f32>,
}

pub fn load(path: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
    parse(&text)
}

pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "window" => {
                let n: Vec<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if n.len() == 4 && n[2] - n[0] >= 200 && n[3] - n[1] >= 150 {
                    s.window = Some([n[0], n[1], n[2], n[3]]);
                }
            }
            "maximized" => s.maximized = v.trim() == "yes",
            "preview_split" => s.split = v.trim().parse::<f32>().ok().filter(|x| (0.2..=0.8).contains(x)),
            _ => {}
        }
    }
    s
}

pub fn to_text(s: &Settings) -> String {
    let mut t = String::from("# ryad settings\n");
    if let Some(w) = s.window {
        t.push_str(&format!("window = {},{},{},{}\n", w[0], w[1], w[2], w[3]));
    }
    t.push_str(&format!("maximized = {}\n", if s.maximized { "yes" } else { "no" }));
    if let Some(x) = s.split {
        t.push_str(&format!("preview_split = {x:.3}\n"));
    }
    t
}

pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("ini.tmp");
    std::fs::write(&tmp, to_text(s))?;
    crate::files::replace_own(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings { window: Some([10, 20, 900, 700]), maximized: true, split: Some(0.5) };
        assert_eq!(parse(&to_text(&s)), s);
        // nonsense sizes are ignored
        assert_eq!(parse("window = 0,0,10,10\n").window, None);
    }
}
