//! Building blocks: panels, chips, display digits, area charts, sparklines,
//! meters and scrollbars, plus number formatting.
use super::{
    canvas::{columns, fit, glyph, Canvas, Color, Rect, Style},
    theme::{fade, glow, ink, BG, LINE, MUTED, RAISED, SURFACE, TEXT},
};
use std::time::Duration;

pub fn bytes(mut value: f64) -> String {
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" { format!("{value:.0} B") } else { format!("{value:.1} {unit}") }
}

pub fn rate(value: f64) -> String {
    format!("{}/s", bytes(value))
}

/// Compact counts: 950, 12.3k, 4.1M.
pub fn count(value: f64) -> String {
    match value {
        v if v >= 1e9 => format!("{:.1}G", v / 1e9),
        v if v >= 1e6 => format!("{:.1}M", v / 1e6),
        v if v >= 1e4 => format!("{:.1}k", v / 1e3),
        v => format!("{v:.0}"),
    }
}

pub fn clock(seconds: u64) -> String {
    format!("{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

/// A rough duration for chart time axes: 45s, 2m 30s, 1h 05m.
pub fn span(seconds: f64) -> String {
    let seconds = seconds.round() as u64;
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s / 60 % 60),
    }
}

/// Split "12.3 MiB/s" into the number and its unit.
pub fn number_unit(text: &str) -> (&str, &str) {
    text.split_once(' ').unwrap_or((text, ""))
}

pub fn proportion(value: f64, scale: f64, width: usize) -> usize {
    if scale <= 0.0 || value <= 0.0 || !value.is_finite() {
        return 0;
    }
    ((value / scale * width as f64).round() as usize).min(width)
}

pub fn take_recent<T>(values: &[T], count: usize) -> &[T] {
    &values[values.len().saturating_sub(count)..]
}

pub fn peak(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::max)
}

/// Word-wrap `text` to `width` cells, breaking long words.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let gap = usize::from(!line.is_empty());
            if columns(&line) + gap + columns(&word) <= width {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            let mut used = 0;
            let mut split = word.len();
            for (index, ch) in word.char_indices() {
                let cells = glyph(ch).map_or(0, |(_, cells)| cells);
                if used + cells > width && used > 0 {
                    split = index;
                    break;
                }
                used += cells;
            }
            let rest = word.split_off(split);
            lines.push(std::mem::replace(&mut word, rest));
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// A glass panel with a rounded frame. The top edge carries the accent and
/// fades into the frame color. Returns the padded content area.
pub fn panel(cv: &mut Canvas, area: Rect, title: &str, meta: &str, accent: Color) -> Rect {
    if area.w < 4 || area.h < 2 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    cv.fill(area, ink(TEXT).on(SURFACE));
    let edge = ink(LINE).on(SURFACE);
    let reach = (area.w as f32 * 0.55).max(1.0);
    for offset in 0..area.w {
        let ch = match offset {
            0 => '╭',
            o if o == area.w - 1 => '╮',
            _ => '─',
        };
        let color = fade(accent, 0.15).mix(LINE, offset as f32 / reach);
        cv.put(area.x + offset, area.y, ch, ink(color).on(SURFACE));
    }
    let bottom = area.bottom() - 1;
    for y in area.y + 1..bottom {
        cv.put(area.x, y, '│', edge);
        cv.put(area.right() - 1, y, '│', edge);
    }
    cv.put(area.x, bottom, '╰', edge);
    cv.hline(area.x + 1, bottom, area.w - 2, '─', edge);
    cv.put(area.right() - 1, bottom, '╯', edge);

    let room = area.w.saturating_sub(6);
    let mut used = 0;
    if !title.is_empty() && room > 0 {
        let title = fit(title, room);
        used = columns(&title) + 2;
        cv.put(area.x + 2, area.y, ' ', edge);
        cv.text(area.x + 3, area.y, &title, ink(accent).on(SURFACE).bold(), room);
        cv.put(area.x + 3 + columns(&title), area.y, ' ', edge);
    }
    let meta_room = room.saturating_sub(used + 2);
    // A frame label is shown whole or not at all.
    if !meta.is_empty() && columns(meta) <= meta_room {
        let meta = fit(meta, meta_room);
        let right = area.right() - 3;
        let start = right - columns(&meta);
        cv.put(start - 1, area.y, ' ', edge);
        cv.text(start, area.y, &meta, ink(MUTED).on(SURFACE), meta_room);
        cv.put(right, area.y, ' ', edge);
    }
    Rect::new(area.x + 2, area.y + 1, area.w - 4, area.h - 2)
}

/// " text " on a colored background. Returns the cells used.
pub fn chip(cv: &mut Canvas, x: usize, y: usize, text: &str, fg: Color, bg: Color, limit: usize) -> usize {
    let label = format!(" {text} ");
    cv.text(x, y, &label, Style::fg(fg).on(bg).bold(), limit)
}

/// A key cap followed by what it does, for the footer.
pub fn keycap(cv: &mut Canvas, x: usize, y: usize, key: &str, label: &str, limit: usize) -> usize {
    let needed = columns(key) + columns(label) + 4;
    if needed > limit {
        return 0;
    }
    let used = chip(cv, x, y, key, TEXT, RAISED, limit);
    used + cv.text(x + used, y, &format!(" {label} "), ink(MUTED).on(BG), limit - used)
}

/// 3×5 pixel glyphs for display numbers. Each row holds `width` bits, MSB left.
fn big_glyph(ch: char) -> Option<(usize, [u8; 5])> {
    Some(match ch {
        '0' => (3, [7, 5, 5, 5, 7]),
        '1' => (3, [2, 6, 2, 2, 7]),
        '2' => (3, [7, 1, 7, 4, 7]),
        '3' => (3, [7, 1, 3, 1, 7]),
        '4' => (3, [5, 5, 7, 1, 1]),
        '5' => (3, [7, 4, 7, 1, 7]),
        '6' => (3, [7, 4, 7, 5, 7]),
        '7' => (3, [7, 1, 1, 2, 2]),
        '8' => (3, [7, 5, 7, 5, 7]),
        '9' => (3, [7, 5, 7, 1, 7]),
        '-' => (3, [0, 0, 7, 0, 0]),
        '.' => (1, [0, 0, 0, 0, 1]),
        ':' => (1, [0, 1, 0, 1, 0]),
        ' ' => (1, [0; 5]),
        _ => return None,
    })
}

pub fn big_width(text: &str) -> usize {
    let glyphs: Vec<_> = text.chars().filter_map(big_glyph).collect();
    glyphs.iter().map(|glyph| glyph.0).sum::<usize>() + glyphs.len().saturating_sub(1)
}

/// Draw a three-row display number with half blocks, brightest at the top.
pub fn big_text(cv: &mut Canvas, x: usize, y: usize, text: &str, color: Color) -> usize {
    let shades = [glow(color), color, fade(color, 0.18)];
    let mut cursor = x;
    for (index, (width, rows)) in text.chars().filter_map(big_glyph).enumerate() {
        if index > 0 {
            cursor += 1;
        }
        for (row, shade) in shades.iter().enumerate() {
            let top = rows[row * 2];
            let bottom = rows.get(row * 2 + 1).copied().unwrap_or(0);
            for col in 0..width {
                let bit = 1 << (width - 1 - col);
                let ch = match (top & bit != 0, bottom & bit != 0) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => continue,
                };
                cv.put(cursor + col, y + row, ch, ink(*shade));
            }
        }
        cursor += width;
    }
    cursor - x
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Grow {
    Up,
    Down,
}

/// Braille: four dot rows per cell, two samples per cell.
fn braille(left: usize, right: usize, grow: Grow) -> char {
    const UP: [[u32; 4]; 2] = [[0x40, 0x04, 0x02, 0x01], [0x80, 0x20, 0x10, 0x08]];
    const DOWN: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let bits = if grow == Grow::Up { &UP } else { &DOWN };
    let dots = |side: usize, fill: usize| bits[side][..fill.min(4)].iter().sum::<u32>();
    char::from_u32(0x2800 + dots(0, left) + dots(1, right)).unwrap_or(' ')
}

/// Blocks: eight levels per cell, one sample per cell.
fn block(fill: usize, grow: Grow) -> char {
    match grow {
        Grow::Up => [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][fill.min(8)],
        Grow::Down => match fill {
            0 => ' ',
            1..=3 => '▔',
            4..=7 => '▀',
            _ => '█',
        },
    }
}

/// A filled area chart, newest sample at the right edge. Cells darken toward
/// the baseline and the crest of each column glows.
pub fn plot(cv: &mut Canvas, rect: Rect, values: &[f64], scale: f64, color: Color, grow: Grow, braille_mode: bool) {
    if rect.is_empty() {
        return;
    }
    let per_cell = if braille_mode { 2 } else { 1 };
    let dots = if braille_mode { 4 } else { 8 };
    let recent = take_recent(values, rect.w * per_cell);
    let total = rect.h * dots;
    let mut levels = vec![None; rect.w * per_cell - recent.len()];
    levels.extend(recent.iter().map(|&value| Some(proportion(value, scale, total).max(usize::from(value > 0.0)))));
    let grid_row = (rect.h >= 5).then_some(rect.h / 2);

    for col in 0..rect.w {
        let pair = &levels[col * per_cell..(col + 1) * per_cell];
        let crest = pair.iter().flatten().copied().max().unwrap_or(0);
        for row in 0..rect.h {
            let y = if grow == Grow::Up { rect.bottom() - 1 - row } else { rect.y + row };
            let x = rect.x + col;
            let fill = |level: Option<usize>| level.unwrap_or(0).saturating_sub(row * dots).min(dots);
            let ch = if braille_mode {
                braille(fill(pair[0]), fill(pair[1]), grow)
            } else {
                block(fill(pair[0]), grow)
            };
            if !matches!(ch, ' ' | '\u{2800}') {
                let depth = (row as f32 + 1.0) / rect.h as f32;
                let mut shade = fade(color, 0.7 * (1.0 - depth));
                if crest > row * dots && crest <= (row + 1) * dots {
                    shade = glow(color);
                }
                cv.put(x, y, ch, ink(shade));
            } else if grid_row == Some(row) && col % 3 == 0 {
                cv.put(x, y, '·', ink(LINE));
            }
        }
    }
}

/// Lift the baseline of a series that never nears zero, such as a socket
/// count, so its movement fills the chart. Returns the shifted values and the
/// offset; a flat series sits at mid height.
pub fn floating(values: &[f64]) -> (Vec<f64>, f64) {
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = peak(values);
    if values.is_empty() || low <= 0.0 {
        return (values.to_vec(), 0.0);
    }
    let floor = (low - (high - low).max(high * 0.02)).max(0.0);
    (values.iter().map(|value| value - floor).collect(), floor)
}

/// A one-row sparkline scaled to its own range.
pub fn sparkline(cv: &mut Canvas, x: usize, y: usize, w: usize, values: &[f64], color: Color) {
    let (recent, _) = floating(take_recent(values, w));
    let top = peak(&recent);
    let start = x + w - recent.len();
    for (index, &value) in recent.iter().enumerate() {
        let level = proportion(value, top, 8).max(usize::from(value > 0.0));
        let (ch, shade) = if level == 0 {
            ('▁', LINE)
        } else {
            (block(level, Grow::Up), fade(color, 0.55 * (1.0 - level as f32 / 8.0)))
        };
        cv.put(start + index, y, ch, ink(shade));
    }
}

/// A thin neon meter with half-cell precision and a gradient along its length.
pub fn meter(cv: &mut Canvas, x: usize, y: usize, w: usize, fraction: f64, from: Color, to: Color) {
    let halves = (fraction.clamp(0.0, 1.0) * (w * 2) as f64).round() as usize;
    let halves = halves.max(usize::from(fraction > 0.0 && w > 0));
    for index in 0..w {
        let color = from.mix(to, index as f32 / (w.max(2) - 1) as f32);
        let (ch, style) = if (index + 1) * 2 <= halves {
            ('━', ink(color))
        } else if index * 2 < halves {
            ('╸', ink(color))
        } else {
            ('─', ink(LINE))
        };
        cv.put(x + index, y, ch, style);
    }
}

/// Two counts shown as one split bar with labels at either end.
pub fn split_bar(cv: &mut Canvas, x: usize, y: usize, w: usize, left: (&str, usize, Color), right: (&str, usize, Color)) {
    split_bar_unit(cv, x, y, w, left, right, "");
}

/// A split bar whose numbers carry a unit, such as `%`.
#[allow(clippy::too_many_arguments)]
pub fn split_bar_unit(cv: &mut Canvas, x: usize, y: usize, w: usize, left: (&str, usize, Color), right: (&str, usize, Color), unit: &str) {
    let left_label = format!("{} {}{unit} ", left.0, left.1);
    let right_label = format!(" {}{unit} {}", right.1, right.0);
    let used = cv.text(x, y, &left_label, ink(left.2).bold(), w);
    let tail = columns(&right_label);
    if used + tail + 4 > w {
        return;
    }
    let bar = w - used - tail;
    let total = left.1 + right.1;
    let share = if total == 0 { 0 } else { proportion(left.1 as f64, total as f64, bar) };
    for index in 0..bar {
        let (ch, color) = match (total, index < share) {
            (0, _) => ('─', LINE),
            (_, true) => ('━', left.2),
            (_, false) => ('━', right.2),
        };
        cv.put(x + used + index, y, ch, ink(color));
    }
    cv.text(x + used + bar, y, &right_label, ink(right.2).bold(), tail);
}

/// A label, value and meter on one row, used for rankings and histograms.
pub fn bar_row(cv: &mut Canvas, row: Rect, label: &str, value: &str, fraction: f64, color: Color) {
    if row.is_empty() {
        return;
    }
    let value_w = columns(value).min(row.w / 3);
    let label_w = (row.w.saturating_sub(value_w + 2) * 2 / 5).clamp(4, 22).min(row.w);
    cv.text(row.x, row.y, &super::canvas::clip(label, label_w), ink(TEXT), label_w);
    let track_x = row.x + label_w + 1;
    let track_w = row.w.saturating_sub(label_w + value_w + 2);
    meter(cv, track_x, row.y, track_w, fraction, fade(color, 0.45), color);
    cv.text_right(row.right(), row.y, value, ink(color).bold(), value_w);
}

/// A scrollbar drawn over a panel's right border.
#[allow(clippy::too_many_arguments)]
pub fn scrollbar(cv: &mut Canvas, x: usize, y: usize, h: usize, start: usize, visible: usize, total: usize, color: Color) {
    if total <= visible || h == 0 {
        return;
    }
    let thumb = (h * visible / total).clamp(1, h);
    let travel = h - thumb;
    let position = (travel * start).div_ceil((total - visible).max(1)).min(travel);
    for offset in 0..h {
        let on = offset >= position && offset < position + thumb;
        let (ch, shade) = if on { ('┃', color) } else { ('│', LINE) };
        cv.put(x, y + offset, ch, ink(shade).on(SURFACE));
    }
}

/// A titled chart panel: the current value in the frame, the window peak in
/// the corner and a time axis underneath.
pub struct Chart<'a> {
    pub title: &'a str,
    pub values: &'a [f64],
    pub color: Color,
    pub format: fn(f64) -> String,
    pub empty: &'a str,
    /// Lift the baseline for series that never near zero.
    pub floating: bool,
}

pub fn chart_panel(cv: &mut Canvas, area: Rect, chart: &Chart<'_>, interval: Duration, braille_mode: bool) {
    let now = chart.values.last().copied().unwrap_or_default();
    let inner = panel(cv, area, chart.title, &format!("now {}", (chart.format)(now)), chart.color);
    if inner.h < 2 || inner.w < 8 {
        return;
    }
    let mut canvas_area = inner;
    let axis = canvas_area.take_bottom(1);
    let per_cell = if braille_mode { 2 } else { 1 };
    let window = take_recent(chart.values, canvas_area.w * per_cell);
    let (shifted, floor) = if chart.floating { floating(window) } else { (window.to_vec(), 0.0) };
    let top = peak(&shifted) + floor;
    plot(cv, canvas_area, &shifted, top - floor, chart.color, Grow::Up, braille_mode);
    if top == 0.0 {
        cv.text_center(canvas_area, canvas_area.y + canvas_area.h / 2, chart.empty, ink(MUTED));
    } else {
        let label = format!(" {} ", (chart.format)(top));
        cv.text_right(canvas_area.right(), canvas_area.y, &label, ink(MUTED).on(SURFACE), canvas_area.w / 2);
    }
    time_axis(cv, axis, window.len(), interval);
}

/// "−2m 30s ┈┈┈┈ now" under a chart.
pub fn time_axis(cv: &mut Canvas, row: Rect, samples: usize, interval: Duration) {
    let label = if samples <= 1 {
        "sampling…".to_string()
    } else {
        format!("-{}", span((samples - 1) as f64 * interval.as_secs_f64()))
    };
    let used = cv.text(row.x, row.y, &label, ink(MUTED), row.w);
    if row.w > used + 6 {
        cv.hline(row.x + used + 1, row.y, row.w - used - 6, '┈', ink(fade(LINE, 0.3)));
        cv.text_right(row.right(), row.y, "now", ink(MUTED), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_fills_each_column_from_its_baseline() {
        assert_eq!(braille(0, 0, Grow::Up), '\u{2800}');
        assert_eq!(braille(4, 4, Grow::Up), '⣿');
        assert_eq!(braille(1, 0, Grow::Up), '⡀');
        assert_eq!(braille(0, 2, Grow::Up), '⢠');
        assert_eq!(braille(1, 0, Grow::Down), '⠁');
        assert_eq!(braille(0, 4, Grow::Down), '⢸');
    }

    #[test]
    fn blocks_grow_in_eighths() {
        assert_eq!(block(8, Grow::Up), '█');
        assert_eq!(block(4, Grow::Up), '▄');
        assert_eq!(block(5, Grow::Down), '▀');
    }

    #[test]
    fn display_numbers_measure_their_width() {
        assert_eq!(big_width("12.5"), 3 + 1 + 3 + 1 + 1 + 1 + 3);
        assert_eq!(big_width(""), 0);
        assert!(big_glyph('x').is_none());
    }

    #[test]
    fn formatting_is_compact() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(rate(1536.0), "1.5 KiB/s");
        assert_eq!(count(12_345.0), "12.3k");
        assert_eq!(span(150.0), "2m 30s");
        assert_eq!(number_unit("1.5 KiB/s"), ("1.5", "KiB/s"));
        assert_eq!(wrap("alpha beta gamma", 10), ["alpha beta", "gamma"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    }
}
