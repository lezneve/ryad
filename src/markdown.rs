//! markdown for the preview: a small parser for the everyday subset and an
//! rtf writer for the preview's rich edit control.
//!
//! supported: atx and setext headings, paragraphs with hard breaks, **bold**,
//! *italic*, ~~strike~~, `code`, fenced and indented code blocks, block
//! quotes, ordered, unordered and task lists (nested by indentation), tables
//! with alignment, horizontal rules, [links](url), <autolinks>, images as
//! their alt text, backslash escapes. not supported: html, footnotes,
//! reference-style links, math. those show as plain text.
//!
//! own code instead of a crate (pulldown-cmark ~6 crates with its build
//! deps): the preview needs this subset turned into rtf, and the parser is
//! a few hundred lines with tests. a link is shown, not opened.

#[derive(Debug, PartialEq, Clone)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Strike(Vec<Inline>),
    Code(String),
    Link(Vec<Inline>, String),
    Image(String),
    Break,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Block {
    Heading(u8, Vec<Inline>),
    Para(Vec<Inline>),
    Code(String),
    Quote(Vec<Block>),
    List { ordered: bool, start: u32, items: Vec<Item> },
    Rule,
    Table { align: Vec<Align>, head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
}

#[derive(Debug, PartialEq, Clone)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

// ---- blocks --------------------------------------------------------------------

/// the control text has `\r` line breaks; crlf and lf are accepted too.
pub fn parse(text: &str) -> Vec<Block> {
    let norm = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = norm.split('\n').collect();
    blocks(&lines)
}

fn indent_of(l: &str) -> usize {
    let mut n = 0;
    for c in l.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4 - n % 4,
            _ => break,
        }
    }
    n
}

/// removes up to `n` columns of leading whitespace.
fn dedent(l: &str, n: usize) -> &str {
    let mut col = 0;
    for (i, c) in l.char_indices() {
        if col >= n {
            return &l[i..];
        }
        match c {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => return &l[i..],
        }
    }
    ""
}

fn is_blank(l: &str) -> bool {
    l.trim().is_empty()
}

fn fence(l: &str) -> Option<(char, usize)> {
    let t = l.trim_start();
    if indent_of(l) > 3 {
        return None;
    }
    for f in ['`', '~'] {
        let n = t.chars().take_while(|&c| c == f).count();
        if n >= 3 && !(f == '`' && t[n..].contains('`')) {
            return Some((f, n));
        }
    }
    None
}

fn heading(l: &str) -> Option<(u8, &str)> {
    if indent_of(l) > 3 {
        return None;
    }
    let t = l.trim_start();
    let n = t.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let rest = &t[n..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let body = rest.trim();
    // closing #s
    let body = body.trim_end_matches('#');
    let body = if body.ends_with(' ') || body.is_empty() { body.trim_end() } else { rest.trim() };
    Some((n as u8, body))
}

fn is_rule(l: &str) -> bool {
    if indent_of(l) > 3 {
        return false;
    }
    let t: String = l.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// a list marker: (ordered, number, width of marker incl. the space after it)
fn list_marker(l: &str) -> Option<(bool, u32, usize)> {
    let ind = indent_of(l);
    let t = dedent(l, ind);
    let mut chars = t.char_indices();
    let (_, c) = chars.next()?;
    if "-*+".contains(c) {
        let rest = &t[1..];
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            if is_rule(l) {
                return None;
            }
            let pad = if rest.is_empty() { 1 } else { 1 + (indent_of(rest)).clamp(1, 4) };
            return Some((false, 0, ind + pad));
        }
        return None;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let after = &t[digits..];
        if (after.starts_with('.') || after.starts_with(')')) && (after.len() == 1 || after[1..].starts_with([' ', '\t'])) {
            let n = t[..digits].parse().unwrap_or(1);
            let rest = &after[1..];
            let pad = if rest.is_empty() { 1 } else { 1 + indent_of(rest).clamp(1, 4) };
            return Some((true, n, ind + digits + pad));
        }
    }
    None
}

/// the text of a list item's first line: indentation, marker and up to four
/// spaces after it removed.
fn after_marker(l: &str) -> &str {
    let t = dedent(l, indent_of(l));
    let marker = if t.starts_with(['-', '*', '+']) { 1 } else { t.chars().take_while(|c| c.is_ascii_digit()).count() + 1 };
    let rest = &t[marker.min(t.len())..];
    let spaces = rest.chars().take_while(|&c| c == ' ').count().min(4);
    &rest[spaces..]
}

fn table_cells(l: &str) -> Vec<&str> {
    let t = l.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    // split on | not escaped
    let mut cells = Vec::new();
    let mut start = 0;
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'|' {
            cells.push(t[start..i].trim());
            start = i + 1;
        }
        i += 1;
    }
    cells.push(t[start..].trim());
    cells
}

fn delimiter_row(l: &str) -> Option<Vec<Align>> {
    if !l.contains('-') {
        return None;
    }
    let cells = table_cells(l);
    let mut out = Vec::new();
    for c in cells {
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let core = c.trim_matches(':');
        if core.is_empty() || !core.chars().all(|ch| ch == '-') {
            return None;
        }
        out.push(match (left, right) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(out)
}

fn starts_block(l: &str) -> bool {
    fence(l).is_some() || heading(l).is_some() || is_rule(l) || l.trim_start().starts_with('>') || list_marker(l).is_some()
}

fn blocks(lines: &[&str]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if is_blank(l) {
            i += 1;
            continue;
        }
        if let Some((f, n)) = fence(l) {
            let ind = indent_of(l);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim_start();
                if t.chars().take_while(|&c| c == f).count() >= n && t.trim_start_matches(f).trim().is_empty() {
                    i += 1;
                    break;
                }
                code.push(dedent(lines[i], ind));
                i += 1;
            }
            out.push(Block::Code(code.join("\n")));
            continue;
        }
        if indent_of(l) >= 4 {
            let mut code = Vec::new();
            while i < lines.len() && (indent_of(lines[i]) >= 4 || is_blank(lines[i])) {
                code.push(dedent(lines[i], 4));
                i += 1;
            }
            while code.last().is_some_and(|c| c.trim().is_empty()) {
                code.pop();
            }
            out.push(Block::Code(code.join("\n")));
            continue;
        }
        if let Some((n, body)) = heading(l) {
            out.push(Block::Heading(n, inlines(body)));
            i += 1;
            continue;
        }
        if is_rule(l) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if l.trim_start().starts_with('>') {
            let mut inner = Vec::new();
            while i < lines.len() && !is_blank(lines[i]) {
                let t = lines[i].trim_start();
                match t.strip_prefix('>') {
                    Some(r) => inner.push(r.strip_prefix(' ').unwrap_or(r)),
                    None if !starts_block(lines[i]) => inner.push(lines[i]), // lazy continuation
                    None => break,
                }
                i += 1;
            }
            out.push(Block::Quote(blocks(&inner)));
            continue;
        }
        if let Some((ordered, start, _)) = list_marker(l) {
            let base = indent_of(l);
            let mut items = Vec::new();
            while i < lines.len() {
                let Some((o, _, width)) = list_marker(lines[i]) else { break };
                if o != ordered || indent_of(lines[i]) != base {
                    break;
                }
                let first = after_marker(lines[i]);
                let mut content = vec![first];
                i += 1;
                while i < lines.len() {
                    let next = lines[i];
                    if is_blank(next) {
                        // the item goes on if the next text line is indented into it
                        let more = lines[i + 1..].iter().find(|x| !is_blank(x)).is_some_and(|x| indent_of(x) >= width);
                        if !more {
                            break;
                        }
                        content.push("");
                        i += 1;
                        continue;
                    }
                    if indent_of(next) >= width {
                        content.push(dedent(next, width));
                    } else if list_marker(next).is_none() && !starts_block(next) && !content.last().is_some_and(|c| c.is_empty()) {
                        content.push(next.trim_start()); // lazy continuation
                    } else {
                        break;
                    }
                    i += 1;
                }
                let mut task = None;
                if let Some(first) = content.first_mut() {
                    for (mark, done) in [("[ ] ", false), ("[x] ", true), ("[X] ", true)] {
                        if let Some(r) = first.strip_prefix(mark) {
                            task = Some(done);
                            *first = r;
                        }
                    }
                }
                items.push(Item { task, blocks: blocks(&content) });
                // a blank line between items keeps the list going
                let mut j = i;
                while j < lines.len() && is_blank(lines[j]) {
                    j += 1;
                }
                if j < lines.len() && list_marker(lines[j]).is_some_and(|(o, _, _)| o == ordered) && indent_of(lines[j]) == base {
                    i = j;
                }
            }
            out.push(Block::List { ordered, start, items });
            continue;
        }
        if l.contains('|')
            && let Some(align) = lines.get(i + 1).and_then(|n| delimiter_row(n))
        {
            let head: Vec<Vec<Inline>> = table_cells(l).into_iter().map(inlines).collect();
            if head.len() == align.len() {
                i += 2;
                let mut rows = Vec::new();
                while i < lines.len() && !is_blank(lines[i]) && lines[i].contains('|') {
                    let mut cells: Vec<Vec<Inline>> = table_cells(lines[i]).into_iter().map(inlines).collect();
                    cells.resize(align.len(), Vec::new());
                    rows.push(cells);
                    i += 1;
                }
                out.push(Block::Table { align, head, rows });
                continue;
            }
        }
        // paragraph (maybe a setext heading)
        let mut para: Vec<&str> = Vec::new();
        while i < lines.len() && !is_blank(lines[i]) {
            let t = lines[i];
            if !para.is_empty() {
                let tt = t.trim();
                if !tt.is_empty() && tt.chars().all(|c| c == '=') {
                    out.push(Block::Heading(1, inlines(&para.join("\n"))));
                    para.clear();
                    i += 1;
                    break;
                }
                if !tt.is_empty() && tt.chars().all(|c| c == '-') {
                    out.push(Block::Heading(2, inlines(&para.join("\n"))));
                    para.clear();
                    i += 1;
                    break;
                }
                if starts_block(t) {
                    break;
                }
            }
            para.push(t);
            i += 1;
        }
        if !para.is_empty() {
            out.push(Block::Para(inlines(&para.join("\n"))));
        }
    }
    out
}

// ---- inlines ---------------------------------------------------------------------

pub fn inlines(s: &str) -> Vec<Inline> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    parse_inline(&chars, &mut out);
    merge_text(out)
}

fn merge_text(v: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for x in v {
        if let (Inline::Text(t), Some(Inline::Text(prev))) = (&x, out.last_mut()) {
            prev.push_str(t);
            continue;
        }
        out.push(x);
    }
    out
}

fn find_seq(c: &[char], from: usize, pat: &[char]) -> Option<usize> {
    let mut i = from;
    while i + pat.len() <= c.len() {
        if c[i] == '\\' {
            i += 2;
            continue;
        }
        if &c[i..i + pat.len()] == pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// the closing `]` of a link text starting after `open`, nesting aware.
fn close_bracket(c: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            '`' => {
                // skip code spans inside link text
                let n = c[i..].iter().take_while(|&&x| x == '`').count();
                if let Some(e) = find_seq(c, i + n, &vec!['`'; n]) {
                    i = e + n - 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_inline(c: &[char], out: &mut Vec<Inline>) {
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Inline>| {
        if !text.is_empty() {
            out.push(Inline::Text(std::mem::take(text)));
        }
    };
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' if i + 1 < c.len() && c[i + 1].is_ascii_punctuation() => {
                text.push(c[i + 1]);
                i += 2;
            }
            '\\' if i + 1 < c.len() && c[i + 1] == '\n' => {
                flush(&mut text, out);
                out.push(Inline::Break);
                i += 2;
            }
            '\n' => {
                // two trailing spaces: a hard break; otherwise a space
                if text.ends_with("  ") {
                    let t = text.trim_end().to_string();
                    text = t;
                    flush(&mut text, out);
                    out.push(Inline::Break);
                } else {
                    let t = text.trim_end().to_string();
                    text = t;
                    text.push(' ');
                }
                i += 1;
                while i < c.len() && c[i] == ' ' {
                    i += 1;
                }
            }
            '`' => {
                let n = c[i..].iter().take_while(|&&x| x == '`').count();
                match find_seq(c, i + n, &vec!['`'; n]).filter(|&e| c.get(e + n) != Some(&'`')) {
                    Some(e) => {
                        flush(&mut text, out);
                        let code: String = c[i + n..e].iter().collect();
                        let code = code.replace('\n', " ");
                        let code = if code.len() > 2 && code.starts_with(' ') && code.ends_with(' ') { code[1..code.len() - 1].to_string() } else { code };
                        out.push(Inline::Code(code));
                        i = e + n;
                    }
                    None => {
                        text.extend(&c[i..i + n]);
                        i += n;
                    }
                }
            }
            '*' | '_' | '~' => {
                let n = c[i..].iter().take_while(|&&x| x == ch).count();
                let next = c.get(i + n).copied();
                let prev = if i == 0 { None } else { Some(c[i - 1]) };
                // an opener must be followed by non-space; `_` not inside words
                let opens = next.is_some_and(|x| !x.is_whitespace()) && !(ch == '_' && prev.is_some_and(|p| p.is_alphanumeric()));
                let width = if ch == '~' {
                    if n >= 2 { 2 } else { 0 }
                } else if n >= 2 {
                    2
                } else {
                    1
                };
                if width == 0 || !opens {
                    text.extend(&c[i..i + n]);
                    i += n;
                    continue;
                }
                let delim = vec![ch; width];
                // closing delimiter: preceded by non-space, not followed by the same char (for width 1)
                let mut j = i + width;
                let mut close = None;
                while let Some(e) = find_seq(c, j, &delim) {
                    let before = c[e - 1];
                    let after = c.get(e + width).copied();
                    let ok = !before.is_whitespace() && (width == 2 || after != Some(ch)) && !(ch == '_' && after.is_some_and(|a| a.is_alphanumeric()));
                    if ok && e > i + width {
                        close = Some(e);
                        break;
                    }
                    j = e + 1;
                }
                match close {
                    Some(e) => {
                        flush(&mut text, out);
                        let mut inner = Vec::new();
                        parse_inline(&c[i + width..e], &mut inner);
                        let inner = merge_text(inner);
                        out.push(match (ch, width) {
                            ('~', _) => Inline::Strike(inner),
                            (_, 2) => Inline::Bold(inner),
                            _ => Inline::Italic(inner),
                        });
                        i = e + width;
                    }
                    None => {
                        text.extend(&c[i..i + width]);
                        i += width;
                    }
                }
            }
            '!' if c.get(i + 1) == Some(&'[') => {
                if let Some(e) = close_bracket(c, i + 1)
                    && c.get(e + 1) == Some(&'(')
                    && let Some(p) = find_seq(c, e + 2, &[')'])
                {
                    flush(&mut text, out);
                    out.push(Inline::Image(c[i + 2..e].iter().collect()));
                    i = p + 1;
                } else {
                    text.push('!');
                    i += 1;
                }
            }
            '[' => {
                if let Some(e) = close_bracket(c, i)
                    && c.get(e + 1) == Some(&'(')
                    && let Some(p) = find_seq(c, e + 2, &[')'])
                {
                    flush(&mut text, out);
                    let mut inner = Vec::new();
                    parse_inline(&c[i + 1..e], &mut inner);
                    let target: String = c[e + 2..p].iter().collect();
                    let url = target.split_whitespace().next().unwrap_or("").trim_matches(['<', '>']).to_string();
                    out.push(Inline::Link(merge_text(inner), url));
                    i = p + 1;
                } else {
                    text.push('[');
                    i += 1;
                }
            }
            '<' => {
                let end = find_seq(c, i + 1, &['>']);
                let inner: Option<String> = end.map(|e| c[i + 1..e].iter().collect());
                match inner.filter(|s| (s.starts_with("http://") || s.starts_with("https://") || s.starts_with("mailto:")) && !s.contains(' ')) {
                    Some(url) => {
                        flush(&mut text, out);
                        out.push(Inline::Link(vec![Inline::Text(url.clone())], url));
                        i = end.unwrap_or(i) + 1;
                    }
                    None => {
                        text.push('<');
                        i += 1;
                    }
                }
            }
            _ => {
                text.push(ch);
                i += 1;
            }
        }
    }
    flush(&mut text, out);
}

// ---- rtf ------------------------------------------------------------------------

/// colours and sizes of one rendering. sizes are font pixels at the
/// window's dpi; `px_to_twips` converts with the control's own (system) dpi.
pub struct Style {
    pub ink: [f32; 3],
    pub ink2: [f32; 3],
    pub accent: [f32; 3],
    pub code_bg: [f32; 3],
    pub hair: [f32; 3],
    pub body_face: String,
    pub head_face: String,
    pub mono_face: String,
    pub body_px: i32,
    /// usable width of the text area, in pixels
    pub width_px: i32,
    pub dpi_scale: f32,
}

struct Rtf<'a> {
    s: String,
    st: &'a Style,
}

const BODY: usize = 0;
const HEAD: usize = 1;
const MONO: usize = 2;

impl Rtf<'_> {
    fn tw(&self, px: f32) -> i32 {
        (px * self.st.dpi_scale) as i32
    }
    fn hp(&self, px: f32) -> i32 {
        // font size in half-points for the control's dpi
        ((px * self.st.dpi_scale) / 10.0).round() as i32
    }
    fn esc(&mut self, t: &str) {
        for ch in t.chars() {
            match ch {
                '\\' | '{' | '}' => {
                    self.s.push('\\');
                    self.s.push(ch);
                }
                '\t' => self.s.push_str("\\tab "),
                '\n' => self.s.push_str("\\line "),
                c if (c as u32) < 0x80 => self.s.push(c),
                c => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        self.s.push_str(&format!("\\u{}?", *u as i16));
                    }
                }
            }
        }
    }
    fn inlines(&mut self, v: &[Inline], code_hp: i32) {
        for x in v {
            match x {
                Inline::Text(t) => self.esc(t),
                Inline::Break => self.s.push_str("\\line "),
                Inline::Bold(i) => {
                    self.s.push_str("{\\b ");
                    self.inlines(i, code_hp);
                    self.s.push('}');
                }
                Inline::Italic(i) => {
                    self.s.push_str("{\\i ");
                    self.inlines(i, code_hp);
                    self.s.push('}');
                }
                Inline::Strike(i) => {
                    self.s.push_str("{\\strike ");
                    self.inlines(i, code_hp);
                    self.s.push('}');
                }
                Inline::Code(t) => {
                    self.s.push_str(&format!("{{\\f{MONO}\\fs{code_hp}\\highlight4 "));
                    // non-breaking spaces: a code span stays on one line
                    self.s.push_str("\\~");
                    self.esc(&t.replace(' ', "\u{a0}"));
                    self.s.push_str("\\~}");
                }
                Inline::Link(i, _) => {
                    self.s.push_str("{\\cf3\\ul ");
                    self.inlines(i, code_hp);
                    self.s.push('}');
                }
                Inline::Image(alt) => {
                    self.s.push_str("{\\cf2 [");
                    self.esc(if alt.is_empty() { "image" } else { alt });
                    self.s.push_str("]}");
                }
            }
        }
    }
    fn para_start(&mut self, sa: f32, left: f32) {
        let body = self.hp(self.st.body_px as f32);
        let sa = self.tw(sa);
        let li = self.tw(left);
        self.s.push_str(&format!("\\pard\\plain\\f{BODY}\\fs{body}\\cf1\\sa{sa}\\sl312\\slmult1\\li{li} "));
    }
    /// a one-cell table: a full-width block with a background or a left bar.
    fn cell_block(&mut self, left: f32, bg: Option<usize>, bar: Option<usize>, pad: f32, f: impl FnOnce(&mut Self)) {
        let w = self.tw(self.st.width_px as f32 - left);
        let l = self.tw(left);
        let gap = self.tw(pad);
        self.s.push_str(&format!(
            "\\pard\\trowd\\trgaph{gap}\\trleft{l}\\trpaddl{gap}\\trpaddr{gap}\\trpaddt{gap}\\trpaddb{gap}\\trpaddfl3\\trpaddfr3\\trpaddft3\\trpaddfb3"
        ));
        if let Some(bg) = bg {
            // the solid fill is a 100 % pattern in the same colour
            self.s.push_str(&format!("\\clcbpat{bg}\\clcfpat{bg}\\clshdng10000"));
        }
        if let Some(bar) = bar {
            self.s.push_str(&format!("\\clbrdrl\\brdrs\\brdrw{}\\brdrcf{bar}", self.tw(3.0).max(20)));
        }
        self.s.push_str(&format!("\\cellx{}", l + w));
        f(self);
        self.s.push_str("\\cell\\row\\pard ");
    }
    fn blocks(&mut self, bs: &[Block], left: f32, in_cell: bool) {
        let body = self.st.body_px as f32;
        let code_hp = self.hp(body * 0.9);
        for (k, b) in bs.iter().enumerate() {
            let last = k + 1 == bs.len();
            let intbl = if in_cell { "\\intbl" } else { "" };
            match b {
                Block::Heading(n, text) => {
                    let scale = [2.0, 1.55, 1.3, 1.12, 1.0, 0.95][(*n as usize - 1).min(5)];
                    let sb = self.tw(body * if *n <= 2 { 1.4 } else { 1.0 });
                    let sa = self.tw(body * 0.5);
                    let hp = self.hp(body * scale);
                    let color = if *n >= 5 { 2 } else { 1 };
                    let face = if *n <= 2 { HEAD } else { BODY };
                    let li = self.tw(left);
                    self.s.push_str(&format!("\\pard\\plain{intbl}\\f{face}\\fs{hp}\\b\\cf{color}\\sb{sb}\\sa{sa}\\li{li}\\keepn "));
                    self.inlines(text, hp);
                    if *n <= 2 && !in_cell {
                        self.s.push_str("\\par ");
                        self.rule(left, 5);
                    } else if !(in_cell && last) {
                        self.s.push_str("\\par ");
                    }
                }
                Block::Para(text) => {
                    self.para_start(body * 0.8, left);
                    if in_cell {
                        self.s.push_str("\\intbl ");
                    }
                    self.inlines(text, code_hp);
                    if !(in_cell && last) {
                        self.s.push_str("\\par ");
                    }
                }
                Block::Code(code) => {
                    if in_cell {
                        // no nested tables: a quote shows its code as mono paragraphs
                        let li = self.tw(left);
                        self.s.push_str(&format!("\\pard\\plain\\intbl\\f{MONO}\\fs{code_hp}\\cf1\\li{li} "));
                        self.esc(code);
                        if !last {
                            self.s.push_str("\\par ");
                        }
                        continue;
                    }
                    self.cell_block(left, Some(4), None, body * 0.75, |r| {
                        r.s.push_str(&format!("\\pard\\plain\\intbl\\f{MONO}\\fs{code_hp}\\cf1\\sl276\\slmult1 "));
                        r.esc(code);
                    });
                    self.spacer(body * 0.6);
                }
                Block::Quote(inner) => {
                    if in_cell {
                        self.blocks(inner, left + body, true);
                        continue;
                    }
                    self.cell_block(left, None, Some(3), body * 0.6, |r| {
                        r.s.push_str("{\\cf2 ");
                        if inner.is_empty() {
                            r.s.push_str("\\pard\\intbl ");
                        }
                        r.blocks(inner, 0.0, true);
                        r.s.push('}');
                    });
                    self.spacer(body * 0.6);
                }
                Block::List { ordered, start, items } => {
                    let hang = body * 1.6;
                    for (n, item) in items.iter().enumerate() {
                        let marker = match (item.task, ordered) {
                            (Some(true), _) => "\u{2611}".to_string(),
                            (Some(false), _) => "\u{2610}".to_string(),
                            (None, true) => format!("{}.", start + n as u32),
                            (None, false) => if left > body * 2.0 { "\u{25e6}" } else { "\u{2022}" }.to_string(),
                        };
                        let li = self.tw(left + hang);
                        let fi = -self.tw(hang);
                        let bodyhp = self.hp(body);
                        let sa = self.tw(body * 0.25);
                        // no 	x here: with an explicit tab stop at the indent,
                        // msftedit 10.0.26100 recursed until the stack overflowed.
                        // with a negative i the tab goes to \li anyway.
                        self.s.push_str(&format!("\\pard\\plain{intbl}\\f{BODY}\\fs{bodyhp}\\cf1\\sa{sa}\\sl288\\slmult1\\li{li}\\fi{fi} {{\\cf2 "));
                        self.esc(&marker);
                        self.s.push_str("}\\tab ");
                        let mut rest: &[Block] = &item.blocks;
                        if let Some(Block::Para(t)) = item.blocks.first() {
                            if item.task == Some(true) {
                                self.s.push_str("{\\cf2 ");
                                self.inlines(t, code_hp);
                                self.s.push('}');
                            } else {
                                self.inlines(t, code_hp);
                            }
                            rest = &item.blocks[1..];
                        }
                        self.s.push_str("\\par ");
                        if !rest.is_empty() {
                            self.blocks(rest, left + hang, in_cell);
                        }
                    }
                    if !in_cell {
                        self.spacer(body * 0.4);
                    }
                }
                Block::Rule => {
                    if !in_cell {
                        self.spacer(body * 0.4);
                        self.rule(left, 5);
                    }
                }
                Block::Table { align, head, rows } => {
                    if in_cell {
                        continue;
                    }
                    self.table(left, align, head, rows, code_hp);
                    self.spacer(body * 0.6);
                }
            }
        }
    }
    /// a thin line across the text: an empty one-row table with a top border.
    fn rule(&mut self, left: f32, color: usize) {
        let w = self.tw(self.st.width_px as f32 - left);
        let l = self.tw(left);
        self.s.push_str(&format!(
            "\\pard\\trowd\\trgaph0\\trleft{l}\\clbrdrt\\brdrs\\brdrw10\\brdrcf{color}\\cellx{}\\pard\\plain\\intbl\\fs4\\sl-40\\slmult0 \\cell\\row\\pard ",
            l + w
        ));
    }
    fn spacer(&mut self, px: f32) {
        let h = -self.tw(px).max(20);
        self.s.push_str(&format!("\\pard\\plain\\fs2\\sl{h}\\slmult0\\par "));
    }
    fn table(&mut self, left: f32, align: &[Align], head: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>], code_hp: i32) {
        let cols = align.len().max(1);
        let total = self.tw(self.st.width_px as f32 - left);
        let l = self.tw(left);
        let pad = self.tw(self.st.body_px as f32 * 0.5);
        let body = self.hp(self.st.body_px as f32);
        let row = |r: &mut Self, cells: &[Vec<Inline>], header: bool| {
            r.s.push_str(&format!("\\pard\\trowd\\trgaph{pad}\\trleft{l}\\trpaddt{}\\trpaddb{}\\trpaddft3\\trpaddfb3", pad / 2, pad / 2));
            for c in 0..cols {
                let edge = |side: &str| format!("\\clbrdr{side}\\brdrs\\brdrw10\\brdrcf5");
                r.s.push_str(&edge("t"));
                r.s.push_str(&edge("b"));
                r.s.push_str(&edge("l"));
                r.s.push_str(&edge("r"));
                if header {
                    r.s.push_str("\\clcbpat4\\clcfpat4\\clshdng10000");
                }
                r.s.push_str(&format!("\\cellx{}", l + total * (c as i32 + 1) / cols as i32));
            }
            for (c, a) in align.iter().enumerate().take(cols) {
                let q = match a {
                    Align::Left => "\\ql",
                    Align::Center => "\\qc",
                    Align::Right => "\\qr",
                };
                r.s.push_str(&format!("\\pard\\plain\\intbl{q}\\f{BODY}\\fs{body}\\cf1 "));
                if header {
                    r.s.push_str("\\b ");
                }
                if let Some(cell) = cells.get(c) {
                    r.inlines(cell, code_hp);
                }
                r.s.push_str("\\cell ");
            }
            r.s.push_str("\\row ");
        };
        row(self, head, true);
        for r in rows {
            row(self, r, false);
        }
        self.s.push_str("\\pard ");
    }
}

fn rtf_color(c: [f32; 3]) -> String {
    let b = |v: f32| (v * 255.0).round() as u8;
    format!("\\red{}\\green{}\\blue{};", b(c[0]), b(c[1]), b(c[2]))
}

/// the whole document as rtf (7-bit ascii, other characters as \uN).
pub fn to_rtf(blocks: &[Block], st: &Style) -> String {
    let mut r = Rtf { s: String::new(), st };
    r.s.push_str("{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl");
    for (i, face) in [&st.body_face, &st.head_face, &st.mono_face].iter().enumerate() {
        r.s.push_str(&format!("{{\\f{i}\\fnil "));
        r.esc(face);
        r.s.push_str(";}");
    }
    r.s.push_str("}{\\colortbl;");
    for c in [st.ink, st.ink2, st.accent, st.code_bg, st.hair] {
        r.s.push_str(&rtf_color(c));
    }
    r.s.push('}');
    r.blocks(blocks, 0.0, false);
    r.s.push('}');
    r.s
}

#[cfg(test)]
mod tests {
    use super::*;
    use Inline::*;

    fn t(s: &str) -> Inline {
        Text(s.into())
    }

    #[test]
    fn inline_styles() {
        assert_eq!(
            inlines("a **b** *c* ~~d~~ `e`"),
            vec![t("a "), Bold(vec![t("b")]), t(" "), Italic(vec![t("c")]), t(" "), Strike(vec![t("d")]), t(" "), Code("e".into())]
        );
        assert_eq!(inlines("**жирний _і курсив_**"), vec![Bold(vec![t("жирний "), Italic(vec![t("і курсив")])])]);
        assert_eq!(inlines("snake_case_name і 2 * 3 * 4"), vec![t("snake_case_name і 2 * 3 * 4")]);
        assert_eq!(inlines("\\*не курсив\\*"), vec![t("*не курсив*")]);
        assert_eq!(
            inlines("[сайт](https://x.org \"t\") і <https://a.b>"),
            vec![Link(vec![t("сайт")], "https://x.org".into()), t(" і "), Link(vec![t("https://a.b")], "https://a.b".into())]
        );
        assert_eq!(inlines("![кіт](cat.png)"), vec![Image("кіт".into())]);
        assert_eq!(inlines("рядок  \nдалі"), vec![t("рядок"), Break, t("далі")]);
        assert_eq!(inlines("рядок\nдалі"), vec![t("рядок далі")]);
        assert_eq!(inlines("``код з ` всередині``"), vec![Code("код з ` всередині".into())]);
        assert_eq!(inlines("незакритий **жир"), vec![t("незакритий **жир")]);
    }

    #[test]
    fn blocks_of_a_note() {
        let md = "# Заголовок\r\rабзац\rпродовження\r\r- один\r- два\r  - вкладений\r\r1. перший\r2. другий\r\r> цитата\r> ще\r\r```rust\rfn main() {}\r```\r\r---\r\r| a | b |\r|:--|--:|\r| 1 | 2 |\r\r- [x] зроблено\r- [ ] ні";
        let b = parse(md);
        assert_eq!(b[0], Block::Heading(1, vec![t("Заголовок")]));
        assert_eq!(b[1], Block::Para(vec![t("абзац продовження")]));
        let Block::List { ordered: false, items, .. } = &b[2] else { panic!("{:?}", b[2]) };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[1].blocks[1], Block::List { items, .. } if items.len() == 1));
        assert!(matches!(&b[3], Block::List { ordered: true, start: 1, items } if items.len() == 2));
        assert_eq!(b[4], Block::Quote(vec![Block::Para(vec![t("цитата ще")])]));
        assert_eq!(b[5], Block::Code("fn main() {}".into()));
        assert_eq!(b[6], Block::Rule);
        assert!(matches!(&b[7], Block::Table { align, rows, .. } if align == &vec![Align::Left, Align::Right] && rows.len() == 1));
        let Block::List { items, .. } = &b[8] else { panic!() };
        assert_eq!((items[0].task, items[1].task), (Some(true), Some(false)));
        assert_eq!(b.len(), 9);
    }

    #[test]
    fn setext_crlf_and_code() {
        let b = parse("Назва\r\n=====\r\n\r\nтекст\r\n---\r\n\r\n    відступ\r\n    код\r\n");
        assert_eq!(b[0], Block::Heading(1, vec![t("Назва")]));
        assert_eq!(b[1], Block::Heading(2, vec![t("текст")]));
        assert_eq!(b[2], Block::Code("відступ\nкод".into()));
        // an unclosed fence runs to the end; # without a space is text
        let b = parse("```\nкод\n#не заголовок");
        assert_eq!(b, vec![Block::Code("код\n#не заголовок".into())]);
        assert_eq!(parse("#тег"), vec![Block::Para(vec![t("#тег")])]);
    }

    #[test]
    fn rtf_is_ascii_and_balanced() {
        let st = Style {
            ink: [1.0; 3],
            ink2: [0.6; 3],
            accent: [0.9, 0.1, 0.1],
            code_bg: [0.2; 3],
            hair: [0.3; 3],
            body_face: "Segoe UI".into(),
            head_face: "Segoe UI".into(),
            mono_face: "Consolas".into(),
            body_px: 15,
            width_px: 600,
            dpi_scale: 15.0,
        };
        let r = to_rtf(&parse("# Привіт 😀 {дужки} \\\n\n> `код`\n\n| a |\n|---|\n| б |"), &st);
        assert!(r.is_ascii());
        assert!(r.contains("\\u-10179?\\u-8704?")); // 😀 as a surrogate pair
        let depth = r
            .chars()
            .fold((0i32, ' '), |(d, prev), c| {
                (
                    if prev == '\\' {
                        d
                    } else if c == '{' {
                        d + 1
                    } else if c == '}' {
                        d - 1
                    } else {
                        d
                    },
                    if prev == '\\' && c == '\\' { ' ' } else { c },
                )
            })
            .0;
        assert_eq!(depth, 0);
    }
}

#[cfg(test)]
mod sample {
    #[test]
    fn sample_note_parses() {
        let parts = [
            "# t\n\ntext **b** `c`.\nlink [s](https://x) 😀.\n",
            "## s\n\n- [x] a\n- [ ] b\n- c\n  - d\n",
            "1. a\n2. b\n",
            "> q\n> w\n",
            "```rust\nfn main() {\n}\n```\n",
            "| a | b | c |\n|:--|--:|:-:|\n| 1 | 2 | 3 |\n",
            "---\n\n### x\n\npara\n",
        ];
        let st = super::Style {
            ink: [1.0; 3],
            ink2: [0.6; 3],
            accent: [0.9, 0.1, 0.1],
            code_bg: [0.2; 3],
            hair: [0.3; 3],
            body_face: "Segoe UI".into(),
            head_face: "Segoe UI".into(),
            mono_face: "Consolas".into(),
            body_px: 22,
            width_px: 700,
            dpi_scale: 10.0,
        };
        for p in parts {
            let b = super::parse(p);
            let _ = super::to_rtf(&b, &st);
        }
        let all = parts.concat().replace(char::from(10), &char::from(13).to_string());
        let _ = super::to_rtf(&super::parse(&all), &st);
        let exact = ["- a", "- b", "  - d", ""].join(&char::from(13).to_string());
        assert!(matches!(super::parse(&exact).as_slice(), [super::Block::List { items, .. }] if items.len() == 2));
    }
}
