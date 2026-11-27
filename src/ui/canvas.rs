//! A cell framebuffer. Views draw into a `Canvas`, which is encoded to ANSI
//! once per frame. Drawing is clipped per cell, so no view can overflow a row,
//! push a border out of line or leave half of a wide glyph behind.
use std::{
    fmt::Write as _,
    os::raw::{c_char, c_int},
};

extern "C" {
    fn wcwidth(character: c_int) -> c_int;
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

/// Use a UTF-8 character locale so `wcwidth` measures box drawing, braille
/// and wide characters. Call once, before any other thread starts.
pub fn init_locale() {
    // Category 0 is LC_CTYPE on glibc and LC_ALL on macOS; both cover wcwidth.
    // SAFETY: called on the main thread before workers start; the strings are NUL terminated.
    unsafe {
        if setlocale(0, b"C.UTF-8\0".as_ptr().cast()).is_null() {
            setlocale(0, b"\0".as_ptr().cast());
        }
    }
}

/// A character and its display width. Control and bidi override characters
/// are dropped so external strings cannot move the cursor or reorder a row.
pub fn glyph(ch: char) -> Option<(char, usize)> {
    if ch == ' ' || ch.is_ascii_graphic() {
        return Some((ch, 1));
    }
    if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
        return None;
    }
    // SAFETY: wcwidth reads only its argument and the process locale.
    let width = unsafe { wcwidth(ch as c_int) };
    if width < 0 { Some(('?', 1)) } else { Some((ch, width as usize)) }
}

pub fn columns(text: &str) -> usize {
    text.chars().filter_map(glyph).map(|(_, width)| width).sum()
}

/// The longest prefix of `text` that fits in `limit` cells.
pub fn fit(text: &str, limit: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for (ch, width) in text.chars().filter_map(glyph) {
        if used + width > limit {
            break;
        }
        used += width;
        out.push(ch);
    }
    out
}

/// Fit `text` to `limit` cells, ending with an ellipsis when it was cut.
pub fn clip(text: &str, limit: usize) -> String {
    if columns(text) <= limit {
        return text.chars().filter(|&ch| glyph(ch).is_some()).collect();
    }
    if limit == 0 {
        return String::new();
    }
    let mut out = fit(text, limit - 1);
    out.push('…');
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// SGR foreground code (30–37, 90–97) for 16-color terminals.
    pub basic: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8, basic: u8) -> Self {
        Self { r, g, b, basic }
    }

    /// Blend toward `other`; `t` = 0 keeps `self`. The 16-color code always
    /// comes from `self`, so a faded accent stays recognisable on basic terminals.
    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Self { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b), basic: self.basic }
    }

    /// Nearest entry in the xterm 256-color cube or grayscale ramp.
    pub fn ansi256(self) -> u8 {
        const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
        let (r, g, b) = (i32::from(self.r), i32::from(self.g), i32::from(self.b));
        let step = |value: i32| -> usize {
            if value < 48 { 0 } else if value < 115 { 1 } else { ((value - 35) / 40) as usize }
        };
        let (ri, gi, bi) = (step(r), step(g), step(b));
        let distance = |x: i32, y: i32, z: i32| (x - r).pow(2) + (y - g).pow(2) + (z - b).pow(2);
        let gray_index = (((r + g + b) / 3 - 8).clamp(0, 230) / 10) as u8;
        let gray = 8 + i32::from(gray_index) * 10;
        if distance(gray, gray, gray) < distance(LEVELS[ri], LEVELS[gi], LEVELS[bi]) {
            232 + gray_index
        } else {
            16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    /// `None` keeps whatever background the cell already has.
    pub bg: Option<Color>,
    pub bold: bool,
}

impl Style {
    pub const fn fg(fg: Color) -> Self {
        Self { fg, bg: None, bold: false }
    }

    pub const fn on(self, bg: Color) -> Self {
        Self { bg: Some(bg), ..self }
    }

    pub const fn bold(self) -> Self {
        Self { bold: true, ..self }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> usize {
        self.x + self.w
    }

    pub fn bottom(self) -> usize {
        self.y + self.h
    }

    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn row(self, index: usize) -> Self {
        Self { y: self.y + index, h: usize::from(index < self.h), ..self }
    }

    /// Remove `rows` from the top and return them.
    pub fn take_top(&mut self, rows: usize) -> Self {
        let rows = rows.min(self.h);
        let top = Self { h: rows, ..*self };
        self.y += rows;
        self.h -= rows;
        top
    }

    /// Remove `rows` from the bottom and return them.
    pub fn take_bottom(&mut self, rows: usize) -> Self {
        let rows = rows.min(self.h);
        self.h -= rows;
        Self { y: self.y + self.h, h: rows, ..*self }
    }

    /// `N` columns of near-equal width separated by `gap` cells.
    pub fn split<const N: usize>(self, gap: usize) -> [Self; N] {
        let inner = self.w.saturating_sub(gap * (N - 1));
        let mut x = self.x;
        std::array::from_fn(|index| {
            let w = inner / N + usize::from(index < inner % N);
            let column = Self { x, w, ..self };
            x += w + gap;
            column
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorMode {
    #[default]
    Off,
    Basic,
    Indexed,
    TrueColor,
}

impl ColorMode {
    pub fn detect(enabled: bool) -> Self {
        if !enabled {
            return Self::Off;
        }
        let terminal = std::env::var("TERM").unwrap_or_default();
        match std::env::var("COLORTERM").as_deref() {
            Ok("truecolor" | "24bit") => Self::TrueColor,
            _ if terminal.contains("256color") => Self::Indexed,
            _ => Self::Basic,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    /// `'\0'` marks the right half of the wide glyph to its left.
    ch: char,
    fg: Color,
    bg: Color,
    bold: bool,
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    base: Color,
    cells: Vec<Cell>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, base: Color, fg: Color) -> Self {
        Self { w, h, base, cells: vec![Cell { ch: ' ', fg, bg: base, bold: false }; w * h] }
    }

    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    fn set(&mut self, x: usize, y: usize, ch: char, width: usize, style: Style) {
        if x >= self.w || y >= self.h || width == 0 {
            return;
        }
        if width == 2 && x + 1 >= self.w {
            return self.set(x, y, ' ', 1, style);
        }
        let row = y * self.w;
        // Overwriting either half of a wide glyph blanks its other half.
        if self.cells[row + x].ch == '\0' && x > 0 {
            self.cells[row + x - 1].ch = ' ';
        }
        if x + width < self.w && self.cells[row + x + width].ch == '\0' {
            self.cells[row + x + width].ch = ' ';
        }
        for offset in 0..width {
            let cell = &mut self.cells[row + x + offset];
            cell.ch = if offset == 0 { ch } else { '\0' };
            cell.fg = style.fg;
            cell.bold = style.bold;
            if let Some(bg) = style.bg {
                cell.bg = bg;
            }
        }
    }

    pub fn put(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if let Some((ch, width)) = glyph(ch) {
            self.set(x, y, ch, width, style);
        }
    }

    /// Draw `text` from `x`, clipped to `limit` cells. Returns the cells used.
    pub fn text(&mut self, x: usize, y: usize, text: &str, style: Style, limit: usize) -> usize {
        let limit = limit.min(self.w.saturating_sub(x));
        let mut used = 0;
        for (ch, width) in text.chars().filter_map(glyph) {
            if width == 0 {
                continue;
            }
            if used + width > limit {
                break;
            }
            self.set(x + used, y, ch, width, style);
            used += width;
        }
        used
    }

    /// Draw `text` so it ends just before column `right`.
    pub fn text_right(&mut self, right: usize, y: usize, text: &str, style: Style, limit: usize) -> usize {
        let text = fit(text, limit.min(right));
        let width = columns(&text);
        self.text(right - width, y, &text, style, width)
    }

    /// Draw `text` centered in `area` on row `y`.
    pub fn text_center(&mut self, area: Rect, y: usize, text: &str, style: Style) -> usize {
        let text = fit(text, area.w);
        let width = columns(&text);
        self.text(area.x + (area.w - width) / 2, y, &text, style, width)
    }

    /// Draw styled pieces one after another. Returns the cells used.
    pub fn spans(&mut self, x: usize, y: usize, spans: &[(&str, Style)], limit: usize) -> usize {
        let mut used = 0;
        for (text, style) in spans {
            used += self.text(x + used, y, text, *style, limit - used);
        }
        used
    }

    pub fn fill(&mut self, area: Rect, style: Style) {
        for y in area.y..area.bottom().min(self.h) {
            for x in area.x..area.right().min(self.w) {
                self.set(x, y, ' ', 1, style);
            }
        }
    }

    pub fn hline(&mut self, x: usize, y: usize, w: usize, ch: char, style: Style) {
        for offset in 0..w {
            self.put(x + offset, y, ch, style);
        }
    }

    /// Fade every cell in `area` toward `toward`, as behind a modal.
    pub fn tint(&mut self, area: Rect, toward: Color, amount: f32) {
        for y in area.y..area.bottom().min(self.h) {
            for x in area.x..area.right().min(self.w) {
                let cell = &mut self.cells[y * self.w + x];
                cell.fg = cell.fg.mix(toward, amount);
                cell.bg = cell.bg.mix(toward, amount);
                cell.bold = false;
            }
        }
    }

    /// Copy rows `from..` of `source` into `target`.
    pub fn blit(&mut self, source: &Canvas, from: usize, target: Rect) {
        let width = source.w.min(target.w);
        for row in 0..target.h {
            let (sy, ty) = (from + row, target.y + row);
            if sy >= source.h || ty >= self.h {
                break;
            }
            for col in 0..width {
                let tx = target.x + col;
                if tx >= self.w {
                    break;
                }
                let mut cell = source.cells[sy * source.w + col];
                // Do not split a wide glyph at either clip edge.
                let cut_head = col + 1 == width && source.cells.get(sy * source.w + col + 1).is_some_and(|next| next.ch == '\0') && col + 1 < source.w;
                if cut_head || (col == 0 && cell.ch == '\0') {
                    cell.ch = ' ';
                }
                self.cells[ty * self.w + tx] = cell;
            }
        }
    }

    /// Rows as plain text, for tests.
    #[cfg(test)]
    pub fn plain_rows(&self) -> Vec<String> {
        self.cells.chunks(self.w.max(1)).take(self.h)
            .map(|row| row.iter().filter(|cell| cell.ch != '\0').map(|cell| cell.ch).collect())
            .collect()
    }

    /// One frame: rows separated by erase-to-end and CRLF, ending with erase-below.
    pub fn encode(&self, mode: ColorMode) -> String {
        let mut out = self.encode_top(mode, self.h);
        out.push_str("\x1b[J");
        out
    }

    /// The first `rows` rows only, for repainting animated chrome in place.
    pub fn encode_top(&self, mode: ColorMode, rows: usize) -> String {
        let rows = rows.min(self.h);
        let colored = mode != ColorMode::Off;
        let mut out = String::with_capacity(self.w * rows * if colored { 14 } else { 3 });
        let base = Cell { ch: ' ', fg: self.base, bg: self.base, bold: false };
        if colored {
            sgr(&mut out, mode, base);
        }
        for y in 0..rows {
            let mut last = None;
            for cell in &self.cells[y * self.w..(y + 1) * self.w] {
                if cell.ch == '\0' {
                    continue;
                }
                // Compare colors as this mode will encode them, so cells that
                // differ only beyond the palette's precision share one escape.
                let key = (encoded(mode, cell.fg), encoded(mode, cell.bg), cell.bold);
                if colored && last != Some(key) {
                    sgr(&mut out, mode, *cell);
                    last = Some(key);
                }
                out.push(cell.ch);
            }
            if colored {
                sgr(&mut out, mode, base);
            }
            out.push_str(if y + 1 < rows { "\x1b[K\r\n" } else { "\x1b[K" });
        }
        out
    }
}

fn encoded(mode: ColorMode, color: Color) -> (u8, u8, u8) {
    match mode {
        ColorMode::Off => (0, 0, 0),
        ColorMode::Basic => (color.basic, 0, 0),
        ColorMode::Indexed => (color.ansi256(), 0, 0),
        ColorMode::TrueColor => (color.r, color.g, color.b),
    }
}

fn sgr(out: &mut String, mode: ColorMode, cell: Cell) {
    out.push_str("\x1b[0");
    if cell.bold {
        out.push_str(";1");
    }
    let (fg, bg) = (cell.fg, cell.bg);
    let _ = match mode {
        ColorMode::Off => Ok(()),
        ColorMode::Basic => write!(out, ";{};{}", fg.basic, bg.basic + 10),
        ColorMode::Indexed => write!(out, ";38;5;{};48;5;{}", fg.ansi256(), bg.ansi256()),
        ColorMode::TrueColor => write!(out, ";38;2;{};{};{};48;2;{};{};{}", fg.r, fg.g, fg.b, bg.r, bg.g, bg.b),
    };
    out.push('m');
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Color = Color::new(200, 200, 200, 37);
    const BASE: Color = Color::new(0, 0, 0, 30);

    #[test]
    fn rects_split_into_columns_with_gaps() {
        let [a, b, c] = Rect::new(2, 0, 20, 1).split::<3>(1);
        assert_eq!((a.x, a.w, b.x, b.w, c.x, c.w), (2, 6, 9, 6, 16, 6));
        let mut area = Rect::new(0, 0, 10, 10);
        assert_eq!(area.take_top(3), Rect::new(0, 0, 10, 3));
        assert_eq!(area.take_bottom(2), Rect::new(0, 8, 10, 2));
        assert_eq!(area, Rect::new(0, 3, 10, 5));
    }

    #[test]
    fn text_is_clipped_to_the_canvas_and_limit() {
        let mut canvas = Canvas::new(8, 2, BASE, INK);
        assert_eq!(canvas.text(5, 0, "abcdef", Style::fg(INK), 99), 3);
        assert_eq!(canvas.text(0, 1, "abcdef", Style::fg(INK), 2), 2);
        assert_eq!(canvas.plain_rows(), ["     abc", "ab      "]);
        canvas.put(99, 99, 'x', Style::fg(INK));
    }

    #[test]
    fn frames_have_exact_rows_and_markers() {
        let canvas = Canvas::new(4, 3, BASE, INK);
        let frame = canvas.encode(ColorMode::Off);
        assert_eq!(frame, "    \x1b[K\r\n    \x1b[K\r\n    \x1b[K\x1b[J");
        assert!(Canvas::new(4, 1, BASE, INK).encode(ColorMode::Indexed).contains(";48;5;16m"));
    }

    #[test]
    fn near_identical_colors_share_an_escape() {
        let mut canvas = Canvas::new(2, 1, BASE, INK);
        canvas.put(0, 0, 'a', Style::fg(Color::new(200, 200, 200, 37)));
        canvas.put(1, 0, 'b', Style::fg(Color::new(201, 199, 200, 37)));
        assert!(canvas.encode(ColorMode::Indexed).contains("ab"));
        assert!(!canvas.encode(ColorMode::TrueColor).contains("ab"));
    }

    #[test]
    fn colors_map_to_the_256_palette() {
        assert_eq!(Color::new(0, 0, 0, 30).ansi256(), 16);
        assert_eq!(Color::new(255, 255, 255, 97).ansi256(), 231);
        assert_eq!(Color::new(0, 255, 255, 96).ansi256(), 51);
        assert_eq!(Color::new(128, 128, 128, 37).ansi256(), 244);
        assert_eq!(Color::new(10, 20, 30, 96).mix(Color::new(30, 40, 50, 30), 0.5), Color::new(20, 30, 40, 96));
    }

    #[test]
    fn clip_marks_cut_text() {
        assert_eq!(clip("network", 5), "netw…");
        assert_eq!(clip("net", 5), "net");
    }
}
