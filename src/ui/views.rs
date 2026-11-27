//! The views. Each draws into the body area under the tabs.
use super::{
    canvas::{clip, columns, Canvas, Color, Rect, Style},
    intel::{measured_rtts, summary, Kind},
    theme::*,
    widgets::*,
    App, Top,
};
use std::collections::{BTreeMap, HashMap};

type Draw<'a> = Box<dyn Fn(&mut Canvas, Rect) + 'a>;

/// A vertical stack of blocks taller than the screen, shown through a
/// scrolling window. Pairs sit side by side when there is room.
struct Page<'a> {
    width: usize,
    blocks: Vec<(usize, Draw<'a>)>,
}

impl<'a> Page<'a> {
    fn new(width: usize) -> Self {
        Self { width, blocks: Vec::new() }
    }

    fn wide(&self) -> bool {
        self.width >= 76
    }

    fn add(&mut self, height: usize, draw: impl Fn(&mut Canvas, Rect) + 'a) {
        self.blocks.push((height, Box::new(draw)));
    }

    fn pair(&mut self, height: usize, left: impl Fn(&mut Canvas, Rect) + 'a, right: impl Fn(&mut Canvas, Rect) + 'a) {
        if self.wide() {
            self.add(height, move |cv, area| {
                let [a, b] = area.split::<2>(1);
                left(cv, a);
                right(cv, b);
            });
        } else {
            self.add(height, left);
            self.add(height, right);
        }
    }

    fn show(self, app: &App, cv: &mut Canvas, body: Rect, accent: Color) {
        let total: usize = self.blocks.iter().map(|block| block.0).sum();
        let mut page = Canvas::new(body.w, total.max(1), BG, TEXT);
        let mut y = 0;
        for (height, draw) in &self.blocks {
            draw(&mut page, Rect::new(0, y, body.w, *height));
            y += height;
        }
        let start = app.scroll.get().min(total.saturating_sub(body.h));
        app.scroll.set(start);
        cv.blit(&page, start, Rect { h: body.h.min(total), ..body });
        if total > body.h {
            scrollbar(cv, body.right() - 1, body.y, body.h, start, body.h, total, accent);
            app.scroll_info.set(Some((start + 1, start + body.h, total)));
        }
    }
}

fn state_color(state: &str) -> Color {
    match state {
        "ESTABLISHED" => LIME,
        "LISTEN" => VIOLET,
        "SYN_SENT" | "SYN_RECV" => CYAN,
        "UNCONNECTED" | "CLOSE" => MUTED,
        _ => AMBER,
    }
}

fn label_value(cv: &mut Canvas, row: Rect, label: &str, value: &str, color: Color) {
    let label_w = 12.min(row.w / 2);
    cv.text(row.x, row.y, label, ink(MUTED), label_w);
    cv.text(row.x + label_w, row.y, &clip(value, row.w - label_w), ink(color), row.w - label_w);
}

/// A tile with a display number, a unit, a small chart and a caption.
#[allow(clippy::too_many_arguments)]
fn hero(cv: &mut Canvas, area: Rect, title: &str, meta: &str, value: &str, unit: &str, color: Color, caption: &str, series: &[f64], braille: bool) {
    let inner = panel(cv, area, title, meta, color);
    if inner.is_empty() {
        return;
    }
    let number_w = big_width(value);
    let unit_w = columns(unit);
    if inner.h >= 3 && number_w + unit_w < inner.w {
        big_text(cv, inner.x, inner.y, value, color);
        cv.text(inner.x + number_w + 1, inner.y + 2, unit, ink(fade(color, 0.15)).bold(), unit_w);
        let free = inner.w - number_w - unit_w - 1;
        if free >= 10 && !series.is_empty() {
            let chart_w = (free - 2).min(28);
            let chart = Rect::new(inner.right() - chart_w, inner.y, chart_w, 3);
            let per_cell = if braille { 2 } else { 1 };
            let (shifted, _) = floating(take_recent(series, chart_w * per_cell));
            area_chart(cv, chart, &shifted, peak(&shifted), color, braille);
        }
        if inner.h >= 4 {
            cv.text(inner.x, inner.y + 3, &clip(caption, inner.w), ink(MUTED), inner.w);
        }
    } else {
        cv.spans(inner.x, inner.y, &[(value, ink(color).bold()), (" ", ink(TEXT)), (unit, ink(fade(color, 0.15)))], inner.w);
        if inner.h >= 2 {
            cv.text(inner.x, inner.y + 1, &clip(caption, inner.w), ink(MUTED), inner.w);
        }
    }
}

fn area_chart(cv: &mut Canvas, rect: Rect, values: &[f64], scale: f64, color: Color, braille: bool) {
    plot(cv, rect, values, scale, color, Grow::Up, braille);
}

impl App {
    fn link_history(&self, name: &str) -> (Vec<f64>, Vec<f64>) {
        let history = self.histories.get(name);
        let down = history.map(|h| h.iter().map(|r| r.0).collect()).unwrap_or_default();
        let up = history.map(|h| h.iter().map(|r| r.1).collect()).unwrap_or_default();
        (down, up)
    }

    fn traffic_totals(&self) -> (u64, u64) {
        self.snapshot.interfaces.iter().filter(|link| self.in_scope(&link.name)).fold((0, 0), |(rx, tx): (u64, u64), link| {
            (rx.saturating_add(link.counters.rx), tx.saturating_add(link.counters.tx))
        })
    }

    // ── 1 Overview ──────────────────────────────────────────────────────────

    pub(super) fn overview(&self, cv: &mut Canvas, mut body: Rect) {
        let (scope, history) = self.traffic();
        let down: Vec<f64> = history.iter().map(|r| r.0).collect();
        let up: Vec<f64> = history.iter().map(|r| r.1).collect();
        let sockets: Vec<f64> = self.sockets.iter().copied().collect();

        if body.w >= 76 && body.h >= 19 {
            let tiles = body.take_top(if body.h >= 24 { 6 } else { 5 });
            let lower = body.take_bottom(if body.h >= 22 { 9 } else { 7 });
            self.hero_row(cv, tiles, &down, &up, &sockets);
            self.waveform(cv, body, scope, &down, &up);
            let [left, right] = lower.split::<2>(1);
            self.connections_panel(cv, left, &sockets);
            self.top_panel(cv, right);
        } else if body.w >= 40 && body.h >= 10 {
            let lower = body.take_bottom(4);
            self.waveform(cv, body, scope, &down, &up);
            let inner = panel(cv, lower, "Sockets", &format!("{} open", self.snapshot.connection_count), VIOLET);
            if !inner.is_empty() {
                let counts = &self.snapshot.sockets;
                cv.text(inner.x, inner.y, &format!("{} est · {} listen", counts.established, counts.listening), ink(TEXT), inner.w / 2);
                let spark_w = inner.w / 2;
                sparkline(cv, inner.right() - spark_w, inner.y, spark_w, &sockets, VIOLET);
                split_bar(cv, inner.x, inner.y + 1, inner.w, ("TCP", counts.tcp, CYAN), ("UDP", counts.udp, MAGENTA));
            }
        } else {
            let inner = panel(cv, body, scope, "", CYAN);
            let rows = [
                ("↓ down", rate(down.last().copied().unwrap_or_default()), &down, CYAN),
                ("↑ up", rate(up.last().copied().unwrap_or_default()), &up, MAGENTA),
                ("sockets", self.snapshot.connection_count.to_string(), &sockets, VIOLET),
            ];
            for (index, (label, value, series, color)) in rows.iter().enumerate().take(inner.h) {
                let y = inner.y + index;
                cv.text(inner.x, y, label, ink(MUTED), 8);
                cv.text(inner.x + 8, y, value, ink(*color).bold(), 12);
                let spark_w = inner.w.saturating_sub(21);
                sparkline(cv, inner.x + 21, y, spark_w, series, *color);
            }
            if inner.h > 3 {
                let counts = &self.snapshot.sockets;
                split_bar(cv, inner.x, inner.y + 3, inner.w, ("TCP", counts.tcp, CYAN), ("UDP", counts.udp, MAGENTA));
            }
        }
    }

    fn hero_row(&self, cv: &mut Canvas, row: Rect, down: &[f64], up: &[f64], sockets: &[f64]) {
        let (received, sent) = self.traffic_totals();
        let now_down = rate(down.last().copied().unwrap_or_default());
        let now_up = rate(up.last().copied().unwrap_or_default());
        let (down_value, down_unit) = number_unit(&now_down);
        let (up_value, up_unit) = number_unit(&now_up);
        let down_caption = format!("peak {} · avg {}", rate(peak(down)), rate(summary(down).average));
        let up_caption = format!("peak {} · avg {}", rate(peak(up)), rate(summary(up).average));
        let counts = &self.snapshot.sockets;
        let socket_caption = format!("TCP {} · UDP {} · {} listening", counts.tcp, counts.udp, counts.listening);
        let socket_count = self.snapshot.connection_count.to_string();
        let tiles = if row.w >= 112 { 4 } else { 3 };

        let draw_traffic = |cv: &mut Canvas, a: Rect, b: Rect, c: Rect| {
            hero(cv, a, "↓ DOWNLOAD", &bytes(received as f64), down_value, down_unit, CYAN, &down_caption, down, self.braille);
            hero(cv, b, "↑ UPLOAD", &bytes(sent as f64), up_value, up_unit, MAGENTA, &up_caption, up, self.braille);
            hero(cv, c, "◇ SOCKETS", &format!("{} est", counts.established), &socket_count, "open", VIOLET, &socket_caption, sockets, self.braille);
        };
        if tiles == 4 {
            let [a, b, c, d] = row.split::<4>(1);
            draw_traffic(cv, a, b, c);
            let rtts: Vec<f64> = self.intel.rtt_ms.iter().copied().collect();
            let now = rtts.last().copied().unwrap_or_default();
            let value = if now > 0.0 { format!("{now:.1}") } else { "--".into() };
            let retransmits = self.intel.retransmits.back().copied().unwrap_or_default();
            let caption = format!("{retransmits:.0} retransmits last sample");
            let color = if now >= 150.0 { AMBER } else { LIME };
            hero(cv, d, "◷ TCP RTT", "mean", &value, "ms", color, &caption, &rtts, self.braille);
        } else {
            let [a, b, c] = row.split::<3>(1);
            draw_traffic(cv, a, b, c);
        }
    }

    /// Download rises above the axis and upload falls below it.
    fn waveform(&self, cv: &mut Canvas, area: Rect, scope: &str, down: &[f64], up: &[f64]) {
        let inner = panel(cv, area, &format!("Traffic · {scope}"), "[ ] switch link", CYAN);
        if inner.h < 3 || inner.w < 16 {
            return;
        }
        let per_cell = if self.braille { 2 } else { 1 };
        let top_h = (inner.h - 1).div_ceil(2);
        let top = Rect::new(inner.x, inner.y, inner.w, top_h);
        let axis_y = inner.y + top_h;
        let bottom = Rect::new(inner.x, axis_y + 1, inner.w, inner.h - top_h - 1);
        let down_peak = peak(take_recent(down, inner.w * per_cell));
        let up_peak = peak(take_recent(up, inner.w * per_cell));
        plot(cv, top, down, down_peak, CYAN, Grow::Up, self.braille);
        plot(cv, bottom, up, up_peak, MAGENTA, Grow::Down, self.braille);

        cv.hline(inner.x, axis_y, inner.w, '─', ink(LINE));
        let label_style = |color: Color| ink(color).on(SURFACE).bold();
        let left = format!("↓ {} ", rate(down.last().copied().unwrap_or_default()));
        let right = format!(" ↑ {}", rate(up.last().copied().unwrap_or_default()));
        let used = cv.text(inner.x, axis_y, &left, label_style(CYAN), inner.w / 2);
        let right_w = cv.text_right(inner.right(), axis_y, &right, label_style(MAGENTA), inner.w / 2);
        let samples = take_recent(down, inner.w * per_cell).len();
        if samples > 1 {
            let window = format!(" ◂ {} ▸ ", span((samples - 1) as f64 * self.interval.as_secs_f64()));
            if columns(&window) + used + right_w + 4 <= inner.w {
                cv.text_center(inner, axis_y, &window, ink(MUTED).on(SURFACE));
            }
        }
        let corner = |cv: &mut Canvas, y: usize, label: String| {
            cv.text_right(inner.right(), y, &format!(" {label}"), ink(MUTED).on(SURFACE), inner.w / 2);
        };
        if down_peak == 0.0 && up_peak == 0.0 {
            cv.text_center(top, top.y + top.h / 2, "listening for traffic…", ink(MUTED));
        } else {
            corner(cv, top.y, format!("peak {}", rate(down_peak)));
            if !bottom.is_empty() {
                corner(cv, bottom.bottom() - 1, format!("peak {}", rate(up_peak)));
            }
        }
    }

    fn connections_panel(&self, cv: &mut Canvas, area: Rect, sockets: &[f64]) {
        let inner = panel(cv, area, "Connections", &format!("{} open", self.snapshot.connection_count), VIOLET);
        if inner.h < 2 {
            return;
        }
        let mut chart = inner;
        let footer = chart.take_bottom(1);
        let per_cell = if self.braille { 2 } else { 1 };
        let (shifted, floor) = floating(take_recent(sockets, chart.w * per_cell));
        let top = peak(&shifted);
        area_chart(cv, chart, &shifted, top, VIOLET, self.braille);
        if top > 0.0 {
            cv.text_right(chart.right(), chart.y, &format!(" {:.0}", top + floor), ink(MUTED).on(SURFACE), 8);
            if floor > 0.0 && chart.h >= 3 {
                cv.text_right(chart.right(), chart.bottom() - 1, &format!(" {floor:.0}"), ink(MUTED).on(SURFACE), 8);
            }
        }
        let counts = &self.snapshot.sockets;
        split_bar(cv, footer.x, footer.y, footer.w, ("TCP", counts.tcp, CYAN), ("UDP", counts.udp, MAGENTA));
    }

    fn top_panel(&self, cv: &mut Canvas, area: Rect) {
        let inner = panel(cv, area, "Top flows", "5 for more", MAGENTA);
        if inner.is_empty() {
            return;
        }
        if self.snapshot.traffic.error.is_some() {
            cv.text(inner.x, inner.y, "TCP counters unavailable", ink(AMBER), inner.w);
            cv.text(inner.x, inner.y + 1, "press 5 for details", ink(MUTED), inner.w);
            return;
        }
        let options = Top::default();
        let entries = options.entries(&self.snapshot);
        let busy: Vec<_> = entries.iter().filter(|entry| entry.down + entry.up > 0.0).take(inner.h).collect();
        if busy.is_empty() {
            cv.text_center(inner, inner.y + inner.h / 2, "no TCP transfers right now", ink(MUTED));
            return;
        }
        let values_w = 26.min(inner.w / 2);
        for (index, entry) in busy.iter().enumerate() {
            let y = inner.y + index;
            let name_w = inner.w - values_w - 1;
            let used = cv.text(inner.x, y, &clip(&entry.label, name_w), ink(TEXT), name_w);
            let hint = entry.hint();
            if !hint.is_empty() && used + 3 < name_w {
                cv.text(inner.x + used + 1, y, &clip(hint, name_w - used - 1), ink(VIOLET), name_w - used - 1);
            }
            let x = inner.right() - values_w;
            cv.text(x, y, &format!("↓ {}", rate(entry.down)), ink(CYAN), values_w / 2);
            cv.text(x + values_w / 2, y, &format!("↑ {}", rate(entry.up)), ink(MAGENTA), values_w - values_w / 2);
        }
    }

    // ── 2 Links ─────────────────────────────────────────────────────────────

    pub(super) fn links(&self, cv: &mut Canvas, body: Rect) {
        // Busy, active links first; loopback last.
        let mut links: Vec<_> = self.snapshot.interfaces.iter().collect();
        links.sort_by_key(|link| {
            let loopback = link.name == "lo" || link.name == "lo0";
            (loopback, link.state != "up", std::cmp::Reverse(link.counters.rx.saturating_add(link.counters.tx)))
        });
        if links.is_empty() {
            let inner = panel(cv, body, "Links", "", CYAN);
            cv.text(inner.x, inner.y, "No interfaces visible.", ink(AMBER), inner.w);
            return;
        }
        let per_row = (body.w / 46).clamp(1, 3);
        let card_h = 9;
        let rows = links.len().div_ceil(per_row);
        let visible = (body.h / card_h).max(1);
        let start = self.scroll.get().min(rows.saturating_sub(visible));
        self.scroll.set(start);
        for (slot, row) in (start..rows.min(start + visible)).enumerate() {
            let line = Rect::new(body.x, body.y + slot * card_h, body.w, card_h.min(body.h));
            let cells: Vec<Rect> = match per_row {
                1 => vec![line],
                2 => line.split::<2>(1).to_vec(),
                _ => line.split::<3>(1).to_vec(),
            };
            for (cell, link) in cells.into_iter().zip(links.iter().copied().skip(row * per_row).take(per_row)) {
                self.link_card(cv, cell, link);
            }
        }
        if rows > visible {
            self.scroll_info.set(Some((start * per_row + 1, ((start + visible) * per_row).min(links.len()), links.len())));
            scrollbar(cv, body.right() - 1, body.y, (visible * card_h).min(body.h), start, visible, rows, CYAN);
        }
    }

    fn link_card(&self, cv: &mut Canvas, area: Rect, link: &crate::collector::Interface) {
        let loopback = link.name == "lo" || link.name == "lo0";
        let up = loopback || link.state == "up";
        let state = if loopback { "loopback".to_string() } else if link.state.is_empty() { "unknown".into() } else { link.state.clone() };
        let accent = if loopback { VIOLET } else if up { CYAN } else { AMBER };
        let meta = link.speed_mbps.map_or_else(String::new, |speed| format!("{speed} Mb/s"));
        let selected = self.selected.as_deref() == Some(link.name.as_str());
        let title = if selected { format!("◈ {}", link.name) } else { link.name.clone() };
        let inner = panel(cv, area, &title, &meta, accent);
        if inner.is_empty() {
            return;
        }
        let (down, up_series) = self.link_history(&link.name);
        let c = &link.counters;
        let status = if up { ("●", LIME) } else { ("○", AMBER) };
        cv.spans(inner.x, inner.y, &[(status.0, ink(status.1)), (" ", ink(TEXT)), (&state, ink(status.1).bold())], inner.w);
        let mac = if link.mac.is_empty() { "—" } else { &link.mac };
        let mtu = if link.mtu.is_empty() { "—" } else { &link.mtu };
        cv.text_right(inner.right(), inner.y, &format!("mtu {mtu}"), ink(MUTED), inner.w / 2);

        let traffic_row = |cv: &mut Canvas, y: usize, arrow: &str, now: f64, total: u64, series: &[f64], color: Color| {
            let value = format!("{arrow} {}", rate(now));
            cv.text(inner.x, y, &value, ink(color).bold(), 14);
            let total = bytes(total as f64);
            let total_w = columns(&total);
            let spark_w = inner.w.saturating_sub(15 + total_w + 1);
            sparkline(cv, inner.x + 15, y, spark_w, series, color);
            cv.text_right(inner.right(), y, &total, ink(MUTED), total_w);
        };
        let rows = inner.h;
        if rows > 1 { traffic_row(cv, inner.y + 1, "↓", link.rx_rate, c.rx, &down, CYAN); }
        if rows > 2 { traffic_row(cv, inner.y + 2, "↑", link.tx_rate, c.tx, &up_series, MAGENTA); }
        if rows > 3 {
            let packets = format!("↓ {}  ↑ {}", count(link.rx_pps), count(link.tx_pps));
            label_value(cv, inner.row(3), "packets/s", &packets, TEXT);
        }
        if rows > 4 {
            let faults = c.rx_errors + c.tx_errors + c.rx_drops + c.tx_drops;
            let text = format!("err {}/{}  drop {}/{}", c.rx_errors, c.tx_errors, c.rx_drops, c.tx_drops);
            label_value(cv, inner.row(4), "faults", &text, if faults > 0 { RED } else { TEXT });
        }
        if rows > 5 {
            label_value(cv, inner.row(5), "mac", mac, TEXT);
        }
        if rows > 6 {
            let addresses = if link.addresses.is_empty() { "no addresses".to_string() } else { link.addresses.join("  ") };
            label_value(cv, inner.row(6), "address", &addresses, VIOLET);
        }
    }

    // ── 3 Sockets ───────────────────────────────────────────────────────────

    pub(super) fn sockets_view(&self, cv: &mut Canvas, mut body: Rect) {
        let counts = &self.snapshot.sockets;
        let sockets: Vec<f64> = self.sockets.iter().copied().collect();
        if body.h >= 16 && body.w >= 60 {
            let strip = body.take_top(7);
            let [left, right] = strip.split::<2>(1);
            chart_panel(cv, left, &Chart {
                title: "Connection history", values: &sockets, color: VIOLET,
                format: |v| format!("{v:.0}"), empty: "no open sockets", floating: true,
            }, self.interval, self.braille);
            let inner = panel(cv, right, "Breakdown", &format!("{} total", self.snapshot.connection_count), VIOLET);
            if !inner.is_empty() {
                split_bar(cv, inner.x, inner.y, inner.w, ("TCP", counts.tcp, CYAN), ("UDP", counts.udp, MAGENTA));
                let mut states: BTreeMap<&str, usize> = BTreeMap::new();
                for socket in self.snapshot.connections.iter().filter(|s| s.protocol.starts_with("TCP")) {
                    *states.entry(socket.state.as_str()).or_default() += 1;
                }
                let waiting: usize = states.iter().filter(|(state, _)| !matches!(**state, "ESTABLISHED" | "LISTEN")).map(|e| *e.1).sum();
                let scale = counts.tcp.max(1) as f64;
                let rows = [("established", counts.established, LIME), ("listening", counts.listening, VIOLET), ("closing / other", waiting, AMBER)];
                for (index, (label, value, color)) in rows.iter().enumerate().take(inner.h.saturating_sub(1)) {
                    bar_row(cv, inner.row(index + 1), label, &value.to_string(), *value as f64 / scale, *color);
                }
            }
        } else if body.h >= 10 {
            let strip = body.take_top(4);
            let inner = panel(cv, strip, "Sockets", &format!("{} open", self.snapshot.connection_count), VIOLET);
            if !inner.is_empty() {
                sparkline(cv, inner.x, inner.y, inner.w, &sockets, VIOLET);
                split_bar(cv, inner.x, inner.y + 1, inner.w, ("TCP", counts.tcp, CYAN), ("UDP", counts.udp, MAGENTA));
            }
        }
        self.socket_table(cv, body);
    }

    fn socket_table(&self, cv: &mut Canvas, area: Rect) {
        let snapshot = &self.snapshot;
        let shown = snapshot.connections.len();
        let meta = if snapshot.connection_count > shown { format!("first {shown} of {}", snapshot.connection_count) } else { format!("{shown} sockets") };
        let mut inner = panel(cv, area, "Socket table", &meta, VIOLET);
        if inner.h < 2 {
            return;
        }
        let measured: HashMap<_, _> = snapshot.traffic.entries.iter()
            .filter(|flow| flow.active && snapshot.traffic.error.is_none())
            .map(|flow| ((flow.inode, flow.local, flow.remote), flow))
            .collect();

        // Columns, widest first to go when space runs out.
        let w = inner.w;
        let show_owner = w >= 62;
        let show_live = w >= 92;
        let show_queue = w >= 116;
        let fixed = 6 + 12 + if show_owner { 22 } else { 0 } + if show_live { 24 } else { 0 } + if show_queue { 16 } else { 0 };
        let endpoint_w = w.saturating_sub(fixed) / 2;
        let mut columns_spec: Vec<(&str, usize)> = vec![("PROTO", 6), ("STATE", 12), ("LOCAL", endpoint_w), ("REMOTE", endpoint_w)];
        if show_owner { columns_spec.push(("PROCESS", 22)); }
        if show_live { columns_spec.push(("LIVE ↓ / ↑", 24)); }
        if show_queue { columns_spec.push(("QUEUE rx/tx", 16)); }

        let header = inner.take_top(1);
        let mut x = header.x;
        for (title, width) in &columns_spec {
            cv.text(x, header.y, title, ink(MUTED).bold(), width.saturating_sub(1));
            x += width;
        }
        if snapshot.connections.is_empty() {
            cv.text_center(inner, inner.y + inner.h / 2, "No TCP or UDP sockets visible", ink(MUTED));
            return;
        }
        let total = snapshot.connections.len();
        let start = self.scroll.get().min(total.saturating_sub(inner.h));
        self.scroll.set(start);
        let end = (start + inner.h).min(total);
        for (offset, socket) in snapshot.connections[start..end].iter().enumerate() {
            let y = inner.y + offset;
            if (start + offset) % 2 == 1 {
                cv.fill(Rect::new(inner.x - 1, y, inner.w + 2, 1), ink(TEXT).on(SURFACE.mix(RAISED, 0.3)));
            }
            let tcp = socket.protocol.starts_with("TCP");
            let mut cells: Vec<(String, Style)> = vec![
                (socket.protocol.clone(), ink(if tcp { CYAN } else { MAGENTA }).bold()),
                (socket.state.clone(), ink(state_color(&socket.state))),
                (socket.local.clone(), ink(TEXT)),
                (socket.remote.clone(), ink(TEXT)),
            ];
            if show_owner {
                let owner = match (&socket.process, socket.pid) {
                    (Some(name), Some(pid)) => format!("{name} · {pid}"),
                    _ => "—".into(),
                };
                cells.push((owner, ink(VIOLET)));
            }
            if show_live {
                let live = socket.local.parse().ok().zip(socket.remote.parse().ok())
                    .and_then(|(local, remote)| measured.get(&(socket.inode, local, remote)))
                    .and_then(|flow| flow.rate)
                    .map_or_else(|| "—".to_string(), |(down, up)| format!("{} / {}", rate(down), rate(up)));
                cells.push((live, ink(CYAN)));
            }
            if show_queue {
                cells.push((format!("{} / {}", bytes(socket.rx_queue as f64), bytes(socket.tx_queue as f64)), ink(MUTED)));
            }
            let mut x = inner.x;
            for ((text, style), (_, width)) in cells.iter().zip(&columns_spec) {
                cv.text(x, y, &clip(text, width.saturating_sub(1)), *style, width.saturating_sub(1));
                x += width;
            }
        }
        scrollbar(cv, area.right() - 1, inner.y, inner.h, start, inner.h, total, VIOLET);
        if total > inner.h {
            self.scroll_info.set(Some((start + 1, end, total)));
        }
    }

    // ── 4 System ────────────────────────────────────────────────────────────

    pub(super) fn system(&self, cv: &mut Canvas, body: Rect) {
        let snapshot = &self.snapshot;
        let mut page = Page::new(body.w);

        let host_rows: Vec<(&str, String)> = vec![
            ("hostname", if snapshot.hostname.is_empty() { "—".into() } else { snapshot.hostname.clone() }),
            ("session", clock(snapshot.uptime)),
            ("sampling", format!("every {} ms", snapshot.interval_ms)),
            ("links", format!("{} ({} up)", snapshot.interfaces.len(), snapshot.interfaces.iter().filter(|l| l.state == "up").count())),
            ("sockets", format!("{} · {} established", snapshot.connection_count, snapshot.sockets.established)),
            ("platform", format!("{} {} · monet {}", std::env::consts::OS, std::env::consts::ARCH, env!("CARGO_PKG_VERSION"))),
        ];
        let dns_rows = snapshot.dns.len().max(1);
        let top_h = host_rows.len().max(dns_rows.min(8)) + 2;
        page.pair(top_h, move |cv, area| {
            let inner = panel(cv, area, "Host", "", LIME);
            for (index, (label, value)) in host_rows.iter().enumerate().take(inner.h) {
                label_value(cv, inner.row(index), label, value, TEXT);
            }
        }, move |cv, area| {
            let inner = panel(cv, area, "DNS resolvers", &format!("{}", snapshot.dns.len()), LIME);
            if snapshot.dns.is_empty() {
                cv.text(inner.x, inner.y, "No resolver addresses reported", ink(MUTED), inner.w);
            }
            for (index, resolver) in snapshot.dns.iter().enumerate().take(inner.h) {
                cv.spans(inner.x, inner.y + index, &[("◆ ", ink(LIME)), (resolver, ink(TEXT))], inner.w);
            }
        });

        if !snapshot.warnings.is_empty() {
            let lines: Vec<String> = snapshot.warnings.iter().flat_map(|warning| wrap(warning, body.w.saturating_sub(8))).collect();
            page.add(lines.len() + 2, move |cv, area| {
                let inner = panel(cv, area, "Notices", &format!("{}", snapshot.warnings.len()), AMBER);
                for (index, line) in lines.iter().enumerate() {
                    cv.text(inner.x, inner.y + index, line, ink(AMBER.mix(TEXT, 0.4)), inner.w);
                }
            });
        }

        let routes = &snapshot.routes;
        page.add(routes.len().max(1) + 3, move |cv, area| {
            let mut inner = panel(cv, area, "IPv4 routes", &format!("{}", routes.len()), CYAN);
            let header = inner.take_top(1);
            let widths = [header.w / 5, header.w * 2 / 5, header.w * 3 / 10];
            let titles = ["LINK", "DESTINATION", "GATEWAY", "METRIC"];
            let mut x = header.x;
            for (index, title) in titles.iter().enumerate() {
                cv.text(x, header.y, title, ink(MUTED).bold(), widths.get(index).copied().unwrap_or(10));
                x += widths.get(index).copied().unwrap_or(0);
            }
            if routes.is_empty() {
                cv.text(inner.x, inner.y, "No IPv4 routes reported", ink(MUTED), inner.w);
            }
            for (index, (link, destination, gateway, metric)) in routes.iter().enumerate().take(inner.h) {
                let y = inner.y + index;
                let default = destination == "0.0.0.0/0";
                let cells = [
                    (link.as_str(), ink(VIOLET)),
                    (if default { "default" } else { destination.as_str() }, ink(if default { CYAN } else { TEXT }).bold()),
                    (gateway.as_str(), ink(TEXT)),
                ];
                let mut x = inner.x;
                for ((text, style), width) in cells.iter().zip(widths) {
                    cv.text(x, y, &clip(text, width.saturating_sub(1)), *style, width.saturating_sub(1));
                    x += width;
                }
                cv.text(x, y, &metric.to_string(), ink(MUTED), inner.right().saturating_sub(x));
            }
        });

        let stats = &snapshot.stats;
        if !stats.is_empty() {
            let per_row = (body.w.saturating_sub(4) / 34).max(1);
            let rows = stats.len().div_ceil(per_row);
            page.add(rows + 2, move |cv, area| {
                let inner = panel(cv, area, "Kernel protocol counters", &format!("{}", stats.len()), VIOLET);
                let cell_w = inner.w / per_row;
                for (index, (name, value)) in stats.iter().enumerate() {
                    let (column, row) = (index / rows, index % rows);
                    let x = inner.x + column * cell_w;
                    let y = inner.y + row;
                    let (group, key) = name.split_once('.').unwrap_or(("", name));
                    let used = cv.spans(x, y, &[(group, ink(fade(VIOLET, 0.2))), (" ", ink(TEXT)), (key, ink(TEXT))], cell_w.saturating_sub(12));
                    let shown = count(*value as f64);
                    let value_w = columns(&shown);
                    if used + value_w + 3 <= cell_w {
                        cv.text_right(x + cell_w - 2, y, &shown, ink(if *value == 0 { MUTED } else { WHITE }), value_w);
                    }
                }
            });
        }
        page.show(self, cv, body, LIME);
    }

    // ── 6 Analytics ─────────────────────────────────────────────────────────

    pub(super) fn analytics(&self, cv: &mut Canvas, body: Rect) {
        let (scope_name, history) = self.traffic();
        let scope = self.scope_names();
        let down: Vec<f64> = history.iter().map(|r| r.0).collect();
        let up: Vec<f64> = history.iter().map(|r| r.1).collect();
        let (moved_down, moved_up) = scope.iter()
            .filter_map(|name| self.intel.links.get(*name))
            .fold((0_u64, 0_u64), |sum, link| (sum.0.saturating_add(link.moved.0), sum.1.saturating_add(link.moved.1)));
        let packets_in = self.intel.combined(&scope, |link| link.packets.iter().map(|p| p.0).collect());
        let packets_out = self.intel.combined(&scope, |link| link.packets.iter().map(|p| p.1).collect());
        let faults = self.intel.combined(&scope, |link| link.faults.iter().copied().collect());
        let (interval, braille) = (self.interval, self.braille);

        let mut page = Page::new(body.w);
        page.add(7, move |cv, area| {
            let inner = panel(cv, area, &format!("Bandwidth · {scope_name}"), "since start", CYAN);
            if inner.is_empty() {
                return;
            }
            let label_w = 12.min(inner.w / 4);
            let cell_w = (inner.w - label_w) / 5;
            let headers = ["NOW", "AVERAGE", "P95", "PEAK", "SESSION"];
            for (index, header) in headers.iter().enumerate() {
                cv.text(inner.x + label_w + index * cell_w, inner.y, header, ink(MUTED).bold(), cell_w.saturating_sub(1));
            }
            for (row, (label, values, moved, color)) in [("↓ download", &down, moved_down, CYAN), ("↑ upload", &up, moved_up, MAGENTA)].into_iter().enumerate() {
                let y = inner.y + 1 + row;
                let s = summary(values);
                cv.text(inner.x, y, label, ink(color).bold(), label_w);
                for (index, value) in [rate(s.now), rate(s.average), rate(s.p95), rate(s.peak), bytes(moved as f64)].iter().enumerate() {
                    let style = if index == 0 { ink(WHITE).bold() } else { ink(TEXT) };
                    cv.text(inner.x + label_w + index * cell_w, y, value, style, cell_w.saturating_sub(1));
                }
            }
            if inner.h >= 5 {
                let total = moved_down.saturating_add(moved_up).max(1);
                let share = (moved_down as f64 / total as f64 * 100.0).round() as usize;
                cv.text(inner.x, inner.y + 4, "session mix", ink(MUTED), label_w);
                split_bar_unit(cv, inner.x + label_w, inner.y + 4, inner.w - label_w, ("↓", share, CYAN), ("↑", 100 - share.min(100), MAGENTA), "%");
            }
        });
        page.pair(9, move |cv, area| {
            chart_panel(cv, area, &Chart { title: "Packets in", values: &packets_in, color: CYAN, format: |v| format!("{} p/s", count(v)), empty: "no packets yet", floating: false }, interval, braille);
        }, move |cv, area| {
            chart_panel(cv, area, &Chart { title: "Packets out", values: &packets_out, color: MAGENTA, format: |v| format!("{} p/s", count(v)), empty: "no packets yet", floating: false }, interval, braille);
        });

        let intel = &self.intel;
        page.pair(9, move |cv, area| {
            chart_panel(cv, area, &Chart { title: "Errors + drops", values: &faults, color: RED, format: |v| format!("{v:.0}"), empty: "links are clean", floating: false }, interval, braille);
        }, move |cv, area| {
            let inner = panel(cv, area, "Session volume by link", "", VIOLET);
            let mut volumes: Vec<_> = intel.links.iter()
                .map(|(name, link)| (name, link.moved.0.saturating_add(link.moved.1)))
                .filter(|volume| volume.1 > 0)
                .collect();
            volumes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let scale = volumes.first().map_or(0, |volume| volume.1) as f64;
            if volumes.is_empty() {
                cv.text_center(inner, inner.y + inner.h / 2, "no bytes counted yet", ink(MUTED));
            }
            for (index, (name, total)) in volumes.into_iter().take(inner.h).enumerate() {
                bar_row(cv, inner.row(index), name, &bytes(total as f64), total as f64 / scale, VIOLET);
            }
        });

        let interfaces = &self.snapshot.interfaces;
        let rated = interfaces.iter().filter(|link| link.speed_mbps.is_some()).count();
        page.add(rated.max(1) + 2, move |cv, area| {
            let inner = panel(cv, area, "Link utilization", "busier direction", AMBER);
            if rated == 0 {
                cv.text(inner.x, inner.y, "No interface reports a link speed (Wi-Fi often does not)", ink(MUTED), inner.w);
            }
            for (index, link) in interfaces.iter().filter(|link| link.speed_mbps.is_some()).enumerate().take(inner.h) {
                let speed = link.speed_mbps.unwrap_or(1);
                let fraction = link.rx_rate.max(link.tx_rate) / (speed as f64 * 125_000.0);
                let value = format!("{:5.1}% of {speed} Mb/s", fraction * 100.0);
                bar_row(cv, inner.row(index), &link.name, &value, fraction, severity(fraction));
            }
        });
        page.show(self, cv, body, CYAN);
    }

    // ── 7 Latency ───────────────────────────────────────────────────────────

    pub(super) fn latency(&self, cv: &mut Canvas, body: Rect) {
        let snapshot = &self.snapshot;
        let rtt: Vec<f64> = self.intel.rtt_ms.iter().copied().collect();
        let retransmits: Vec<f64> = self.intel.retransmits.iter().copied().collect();
        let (interval, braille) = (self.interval, self.braille);
        let mut page = Page::new(body.w);

        page.pair(9, move |cv, area| {
            chart_panel(cv, area, &Chart { title: "Mean TCP round-trip", values: &rtt, color: LIME, format: |v| format!("{v:.1} ms"), empty: "no RTT samples", floating: false }, interval, braille);
        }, move |cv, area| {
            chart_panel(cv, area, &Chart { title: "Retransmits per sample", values: &retransmits, color: AMBER, format: |v| format!("{v:.0}"), empty: "no retransmits", floating: false }, interval, braille);
        });

        page.pair(8, move |cv, area| {
            let inner = panel(cv, area, "RTT histogram", "remote peers", LIME);
            let buckets = [("< 5 ms", 5.0, LIME), ("5 – 20 ms", 20.0, LIME), ("20 – 50 ms", 50.0, CYAN), ("50 – 100 ms", 100.0, AMBER), ("100 – 300 ms", 300.0, AMBER), ("≥ 300 ms", f64::INFINITY, RED)];
            let mut counts = [0_usize; 6];
            for value in measured_rtts(snapshot) {
                counts[buckets.iter().position(|bucket| value < bucket.1).unwrap_or(5)] += 1;
            }
            let scale = counts.iter().copied().max().unwrap_or(0).max(1) as f64;
            for (index, ((label, _, color), n)) in buckets.iter().zip(counts).enumerate().take(inner.h) {
                bar_row(cv, inner.row(index), label, &n.to_string(), n as f64 / scale, *color);
            }
        }, move |cv, area| {
            let inner = panel(cv, area, "TCP states", "", VIOLET);
            let mut states: BTreeMap<String, usize> = BTreeMap::new();
            for socket in snapshot.connections.iter().filter(|s| s.protocol.starts_with("TCP")) {
                *states.entry(socket.state.clone()).or_default() += 1;
            }
            ranked(cv, inner, states, None);
        });

        let mut ports: BTreeMap<String, usize> = BTreeMap::new();
        let mut processes: BTreeMap<String, usize> = BTreeMap::new();
        for socket in &snapshot.connections {
            if let Some(name) = &socket.process {
                *processes.entry(name.clone()).or_default() += 1;
            }
            if socket.state == "ESTABLISHED" {
                if let Ok(remote) = socket.remote.parse::<std::net::SocketAddr>() {
                    *ports.entry(service(remote.port())).or_default() += 1;
                }
            }
        }
        page.pair(10, move |cv, area| {
            let inner = panel(cv, area, "Top remote ports", "established", CYAN);
            ranked(cv, inner, ports.clone(), Some(CYAN));
        }, move |cv, area| {
            let inner = panel(cv, area, "Sockets by process", "", MAGENTA);
            ranked(cv, inner, processes.clone(), Some(MAGENTA));
        });

        let mut peers: HashMap<_, (u32, Option<&str>)> = HashMap::new();
        if snapshot.traffic.error.is_none() {
            for flow in snapshot.traffic.entries.iter().filter(|f| f.active && f.rtt_us > 0 && !f.loopback()) {
                let entry = peers.entry(flow.remote).or_insert((0, flow.process.as_deref()));
                entry.0 = entry.0.max(flow.rtt_us);
            }
        }
        let mut peers: Vec<_> = peers.into_iter().collect();
        peers.sort_unstable_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        peers.truncate(8);
        page.add(peers.len().max(1) + 2, move |cv, area| {
            let inner = panel(cv, area, "Slowest peers", "worst RTT first", AMBER);
            if peers.is_empty() {
                cv.text(inner.x, inner.y, "No remote TCP peers with RTT samples", ink(MUTED), inner.w);
            }
            let worst = peers.first().map_or(1.0, |peer| f64::from(peer.1 .0));
            for (index, (remote, (rtt, process))) in peers.iter().enumerate().take(inner.h) {
                let y = inner.y + index;
                let ms = f64::from(*rtt) / 1000.0;
                let color = if ms >= 300.0 { RED } else if ms >= 100.0 { AMBER } else { LIME };
                cv.text(inner.x, y, &format!("{ms:>9.2} ms"), ink(color).bold(), 12);
                let meter_w = (inner.w / 5).min(24);
                meter(cv, inner.x + 13, y, meter_w, f64::from(*rtt) / worst, fade(color, 0.5), color);
                let label = match process { Some(name) => format!("{remote}  {name}"), None => remote.to_string() };
                let x = inner.x + 14 + meter_w;
                cv.text(x, y, &clip(&label, inner.right().saturating_sub(x)), ink(TEXT), inner.right().saturating_sub(x));
            }
        });
        page.show(self, cv, body, LIME);
    }

    // ── 8 Events ────────────────────────────────────────────────────────────

    pub(super) fn events(&self, cv: &mut Canvas, body: Rect) {
        let events = &self.intel.events;
        let total = events.len();
        let mut inner = panel(cv, body, "Event log", &format!("{total} events · newest first"), VIOLET);
        if inner.is_empty() {
            return;
        }
        let color = |kind: Kind| match kind {
            Kind::Connect => CYAN,
            Kind::Listen => VIOLET,
            Kind::Link => LIME,
            Kind::Fault => RED,
            Kind::Spike => AMBER,
        };
        let summary_row = inner.take_top(1);
        let mut x = summary_row.x;
        for kind in Kind::ALL {
            let n = events.iter().filter(|event| event.kind == kind).count();
            let label = format!("{} {n}", kind.label());
            let used = chip(cv, x, summary_row.y, &label, color(kind), RAISED, summary_row.right().saturating_sub(x));
            x += used + 1;
        }
        inner.take_top(1);
        if total == 0 {
            cv.text_center(inner, inner.y + inner.h / 3, "watching for new connections, listeners,", ink(MUTED));
            cv.text_center(inner, inner.y + inner.h / 3 + 1, "link changes, faults and traffic spikes…", ink(MUTED));
            return;
        }
        let start = self.scroll.get().min(total.saturating_sub(inner.h));
        self.scroll.set(start);
        for (offset, event) in events.iter().rev().skip(start).take(inner.h).enumerate() {
            let y = inner.y + offset;
            let tone = color(event.kind);
            let stamp = format!("+{}", clock(event.uptime));
            cv.text(inner.x, y, &stamp, ink(MUTED), 10);
            cv.put(inner.x + 10, y, '│', ink(fade(tone, 0.5)));
            cv.put(inner.x + 12, y, '●', ink(tone));
            cv.text(inner.x + 14, y, event.kind.label(), ink(tone).bold(), 8);
            cv.text(inner.x + 22, y, &clip(&event.text, inner.w.saturating_sub(22)), ink(TEXT), inner.w.saturating_sub(22));
        }
        scrollbar(cv, body.right() - 1, inner.y, inner.h, start, inner.h, total, VIOLET);
        if total > inner.h {
            self.scroll_info.set(Some((start + 1, (start + inner.h).min(total), total)));
        }
    }
}

/// Counts as bars, largest first. `None` colors each row by its TCP state.
fn ranked(cv: &mut Canvas, inner: Rect, counts: BTreeMap<String, usize>, color: Option<Color>) {
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if counts.is_empty() {
        cv.text_center(inner, inner.y + inner.h / 2, "nothing to rank yet", ink(MUTED));
        return;
    }
    let scale = counts[0].1.max(1) as f64;
    for (index, (label, n)) in counts.iter().enumerate().take(inner.h) {
        let tone = color.unwrap_or_else(|| state_color(label));
        bar_row(cv, inner.row(index), label, &n.to_string(), *n as f64 / scale, tone);
    }
}

fn service(port: u16) -> String {
    let name = match port {
        22 => "ssh",
        25 | 465 | 587 => "smtp",
        53 => "dns",
        80 => "http",
        143 | 993 => "imap",
        443 => "https",
        853 => "dot",
        3306 => "mysql",
        5432 => "postgres",
        6379 => "redis",
        _ => "",
    };
    if name.is_empty() { port.to_string() } else { format!("{port} {name}") }
}
