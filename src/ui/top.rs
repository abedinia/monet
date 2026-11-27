//! View 5: the busiest TCP transfers, ranked by connection, peer or process.
use super::{
    canvas::{clip, columns, fit, Canvas, Rect, Style},
    theme::{fade, ink, AMBER, CYAN, LIME, MAGENTA, MUTED, RAISED, SURFACE, TEXT, VIOLET, WHITE},
    widgets::{bytes, chip, meter, panel, rate, scrollbar},
};
use crate::{
    collector::Snapshot,
    flows::{peer_ip, Flow},
};
use std::{
    cell::Cell,
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Group {
    #[default]
    Connection,
    Peer,
    Process,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Self::Connection => "connections",
            Self::Peer => "remote IPs",
            Self::Process => "processes",
        }
    }

    fn key(self, flow: &Flow) -> GroupKey {
        match self {
            Self::Connection => GroupKey::Connection(flow.remote, flow.local, flow.inode, flow.cookie),
            Self::Peer => GroupKey::Peer(peer_ip(flow.remote.ip())),
            Self::Process => GroupKey::Process(flow.process.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GroupKey {
    Connection(SocketAddr, SocketAddr, u64, u64),
    Peer(IpAddr),
    Process(Option<String>),
}

impl GroupKey {
    fn label(&self) -> String {
        match self {
            Self::Connection(remote, ..) => remote.to_string(),
            Self::Peer(ip) => ip.to_string(),
            Self::Process(name) => name.as_deref().unwrap_or("unknown process").into(),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    Download,
    Upload,
    #[default]
    Total,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Self::Download => "↓ download",
            Self::Upload => "↑ upload",
            Self::Total => "↕ total",
        }
    }

    fn value(self, entry: &Entry<'_>) -> f64 {
        match self {
            Self::Download => entry.down,
            Self::Upload => entry.up,
            Self::Total => entry.down + entry.up,
        }
    }
}

#[derive(Default)]
pub struct Top {
    group: Group,
    sort: Sort,
    session: bool,
    include_loopback: bool,
    cursor: Cell<usize>,
    detail_scroll: Cell<usize>,
    detail: Option<GroupKey>,
}

pub struct Entry<'a> {
    pub label: String,
    key: GroupKey,
    flows: Vec<&'a Flow>,
    pub down: f64,
    pub up: f64,
    measured: bool,
}

impl Entry<'_> {
    /// The owning process, shown beside a single connection.
    pub fn hint(&self) -> &str {
        match self.key {
            GroupKey::Connection(..) => self.flows[0].process.as_deref().unwrap_or(""),
            _ => "",
        }
    }
}

impl Top {
    fn reset(&mut self) {
        self.cursor.set(0);
        self.close_detail();
    }

    pub fn group(&mut self) {
        self.group = match self.group {
            Group::Connection => Group::Peer,
            Group::Peer => Group::Process,
            Group::Process => Group::Connection,
        };
        self.reset();
    }

    pub fn sort(&mut self, sort: Sort) {
        self.sort = sort;
        self.reset();
    }

    pub fn period(&mut self) {
        self.session = !self.session;
        self.reset();
    }

    pub fn loopback(&mut self) {
        self.include_loopback = !self.include_loopback;
        self.reset();
    }

    pub fn scroll(&mut self, direction: isize) {
        let cursor = if self.detail.is_some() { &self.detail_scroll } else { &self.cursor };
        cursor.set(cursor.get().saturating_add_signed(direction));
    }

    fn close_detail(&mut self) {
        self.detail = None;
        self.detail_scroll.set(0);
    }

    pub fn inspect(&mut self, snapshot: &Snapshot) {
        if self.detail.is_some() {
            self.close_detail();
        } else if let Some(entry) = self.entries(snapshot).get(self.cursor.get()) {
            self.detail = Some(entry.key.clone());
        }
    }

    fn covers_loopback(&self, snapshot: &Snapshot) -> bool {
        self.include_loopback || snapshot.interfaces.iter().all(|iface| iface.name == "lo")
    }

    pub fn entries<'a>(&self, snapshot: &'a Snapshot) -> Vec<Entry<'a>> {
        let include_loopback = self.covers_loopback(snapshot);
        let mut groups = BTreeMap::new();
        for flow in &snapshot.traffic.entries {
            if !include_loopback && flow.loopback() {
                continue;
            }
            let values = if self.session {
                if flow.session.download == 0 && flow.session.upload == 0 {
                    continue;
                }
                Some((flow.session.download as f64, flow.session.upload as f64))
            } else {
                if !flow.active {
                    continue;
                }
                flow.rate
            };
            let key = self.group.key(flow);
            let entry = groups.entry(key.clone()).or_insert_with(|| Entry {
                label: key.label(),
                key,
                flows: Vec::new(),
                down: 0.0,
                up: 0.0,
                measured: false,
            });
            if let Some((down, up)) = values {
                entry.down += down;
                entry.up += up;
                entry.measured = true;
            }
            entry.flows.push(flow);
        }
        let mut entries: Vec<_> = groups.into_values().collect();
        entries.sort_unstable_by(|a, b| {
            self.sort.value(b).total_cmp(&self.sort.value(a))
                .then_with(|| Sort::Total.value(b).total_cmp(&Sort::Total.value(a)))
                .then_with(|| b.measured.cmp(&a.measured))
                .then_with(|| a.key.cmp(&b.key))
        });
        entries
    }

    pub fn render(&self, cv: &mut Canvas, area: Rect, snapshot: &Snapshot) -> Option<(usize, usize, usize)> {
        if self.detail.is_some() {
            return self.render_detail(cv, area, snapshot);
        }
        let entries = self.entries(snapshot);
        let meta = format!("{} {}", entries.len(), self.group.label());
        let mut inner = panel(cv, area, "Flows · TCP", &meta, MAGENTA);
        if inner.is_empty() {
            return None;
        }

        // Mode switches, as a row of chips.
        let bar = inner.take_top(1);
        let mut x = bar.x;
        for (label, on) in [("LIVE", !self.session), ("SESSION", self.session)] {
            let (fg, bg) = if on { (SURFACE, MAGENTA) } else { (MUTED, fade(RAISED, 0.3)) };
            let label = if on { format!("● {label}") } else { label.to_string() };
            x += chip(cv, x, bar.y, &label, fg, bg, bar.right().saturating_sub(x));
        }
        x += 1;
        let loopback = if self.covers_loopback(snapshot) { "loopback on" } else { "loopback off" };
        for label in [self.group.label(), self.sort.label(), loopback] {
            x += chip(cv, x, bar.y, label, TEXT, RAISED, bar.right().saturating_sub(x)) + 1;
        }
        inner.take_top(1);

        if let Some(error) = &snapshot.traffic.error {
            cv.text(inner.x, inner.y, &format!("TCP counters unavailable: {error}"), ink(AMBER), inner.w);
            inner.take_top(2);
            if !self.session {
                return None;
            }
        }
        if entries.is_empty() {
            let message = if self.session { "No transfer bytes observed yet" } else { "No matching TCP peers right now" };
            cv.text_center(inner, inner.y + inner.h / 3, message, ink(MUTED));
            return None;
        }

        let wide = inner.w >= 84;
        let rows_per = if wide { 1 } else { 2 };
        if wide {
            let header = inner.take_top(1);
            let layout = Columns::new(header.w);
            let name = match self.group { Group::Connection => "REMOTE", Group::Peer => "PEER", Group::Process => "PROCESS" };
            let style = ink(MUTED).bold();
            cv.text(header.x, header.y, " #", style, 3);
            cv.text(header.x + layout.name_x, header.y, name, style, layout.name_w);
            if self.group == Group::Connection {
                cv.text(header.x + layout.hint_x, header.y, "PROCESS", style, layout.hint_w);
            }
            cv.text(header.x + layout.down_x, header.y, if self.session { "↓ RECEIVED" } else { "↓ DOWNLOAD" }, style, layout.meter_w);
            cv.text(header.x + layout.up_x, header.y, if self.session { "↑ SENT" } else { "↑ UPLOAD" }, style, layout.meter_w);
        }

        let capacity = (inner.h / rows_per).max(1);
        let cursor = self.cursor.get().min(entries.len() - 1);
        self.cursor.set(cursor);
        let start = cursor.saturating_sub(capacity - 1).min(entries.len().saturating_sub(capacity));
        let end = (start + capacity).min(entries.len());
        let scale = entries.iter().map(|entry| entry.down.max(entry.up)).fold(0.0, f64::max);
        for (offset, entry) in entries[start..end].iter().enumerate() {
            let index = start + offset;
            let row = Rect::new(inner.x, inner.y + offset * rows_per, inner.w, rows_per);
            draw_entry(cv, row, index, entry, index == cursor, scale, self.session, wide);
        }
        scrollbar(cv, area.right() - 1, inner.y, inner.h, start, capacity, entries.len(), MAGENTA);
        Some((start + 1, end, entries.len()))
    }

    fn render_detail(&self, cv: &mut Canvas, area: Rect, snapshot: &Snapshot) -> Option<(usize, usize, usize)> {
        let key = self.detail.as_ref()?;
        let title = format!("Transfer · {}", key.label());
        let inner = panel(cv, area, &title, "enter · back", MAGENTA);
        if inner.is_empty() {
            return None;
        }
        let mut flows: Vec<_> = snapshot.traffic.entries.iter()
            .filter(|flow| self.group.key(flow) == *key && (self.covers_loopback(snapshot) || !flow.loopback()))
            .collect();
        flows.sort_unstable_by(|a, b| {
            b.active.cmp(&a.active)
                .then_with(|| b.session.download.saturating_add(b.session.upload).cmp(&a.session.download.saturating_add(a.session.upload)))
                .then_with(|| a.local.cmp(&b.local))
        });

        let mut lines: Vec<Vec<(String, Style)>> = Vec::new();
        if let Some(error) = &snapshot.traffic.error {
            lines.push(vec![(format!("TCP counters unavailable: {error}"), ink(AMBER))]);
        }
        for flow in flows {
            let (state, color) = if flow.active { (flow.state_label(), LIME) } else { ("CLOSED", MUTED) };
            let owner = flow.process.as_deref().unwrap_or("unknown process");
            let pid = flow.pid.map_or_else(String::new, |pid| format!("  pid {pid}"));
            lines.push(vec![
                (format!("● {state}"), ink(color).bold()),
                (format!("   {owner}"), ink(WHITE).bold()),
                (pid, ink(MUTED)),
            ]);
            lines.push(vec![
                ("  local  ".into(), ink(MUTED)), (flow.local.to_string(), ink(TEXT)),
                ("  →  ".into(), ink(VIOLET)),
                ("remote  ".into(), ink(MUTED)), (flow.remote.to_string(), ink(TEXT)),
            ]);
            if let Some((down, up)) = flow.rate {
                lines.push(vec![
                    ("  live     ".into(), ink(MUTED)),
                    (format!("↓ {:<14}", rate(down)), ink(CYAN).bold()),
                    (format!("↑ {}", rate(up)), ink(MAGENTA).bold()),
                ]);
            }
            lines.push(vec![
                ("  session  ".into(), ink(MUTED)),
                (format!("↓ {:<14}", bytes(flow.session.download as f64)), ink(CYAN)),
                (format!("↑ {}", bytes(flow.session.upload as f64)), ink(MAGENTA)),
            ]);
            lines.push(vec![
                ("  socket   ".into(), ink(MUTED)),
                (format!("↓ {:<14}", bytes(flow.lifetime.download as f64)), ink(fade(CYAN, 0.3))),
                (format!("↑ {} acked", bytes(flow.lifetime.upload as f64)), ink(fade(MAGENTA, 0.3))),
            ]);
            let rtt = if flow.rtt_us == 0 { "—".into() } else { format!("{:.2} ms", f64::from(flow.rtt_us) / 1000.0) };
            lines.push(vec![
                ("  rtt      ".into(), ink(MUTED)), (format!("{rtt:<16}"), ink(TEXT)),
                ("retransmits ".into(), ink(MUTED)), (flow.retransmits.to_string(), ink(if flow.retransmits > 0 { AMBER } else { TEXT })),
            ]);
            lines.push(Vec::new());
        }
        if lines.is_empty() {
            lines.push(vec![("This transfer is no longer in the sample.".into(), ink(MUTED))]);
        }

        let start = self.detail_scroll.get().min(lines.len().saturating_sub(inner.h));
        self.detail_scroll.set(start);
        let end = (start + inner.h).min(lines.len());
        for (offset, line) in lines[start..end].iter().enumerate() {
            let spans: Vec<_> = line.iter().map(|(text, style)| (text.as_str(), *style)).collect();
            cv.spans(inner.x, inner.y + offset, &spans, inner.w);
        }
        scrollbar(cv, area.right() - 1, inner.y, inner.h, start, inner.h, lines.len(), MAGENTA);
        (lines.len() > inner.h).then_some((start + 1, end, lines.len()))
    }
}

/// Column positions for the one-row-per-entry layout.
struct Columns {
    name_x: usize,
    name_w: usize,
    hint_x: usize,
    hint_w: usize,
    down_x: usize,
    up_x: usize,
    meter_w: usize,
}

impl Columns {
    fn new(width: usize) -> Self {
        let meter_w = (width * 23 / 100).max(16);
        let hint_w = 16;
        let name_x = 4;
        let name_w = width.saturating_sub(name_x + hint_w + 2 * meter_w + 6);
        let hint_x = name_x + name_w + 2;
        let down_x = hint_x + hint_w + 2;
        Self { name_x, name_w, hint_x, hint_w, down_x, up_x: down_x + meter_w + 2, meter_w }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_entry(cv: &mut Canvas, row: Rect, index: usize, entry: &Entry<'_>, selected: bool, scale: f64, session: bool, wide: bool) {
    if selected {
        cv.fill(Rect::new(row.x - 1, row.y, row.w + 2, row.h), ink(TEXT).on(RAISED));
        for line in 0..row.h {
            cv.put(row.x - 2, row.y + line, '▌', ink(MAGENTA).on(SURFACE));
        }
    }
    let name_style = if selected { ink(WHITE).bold() } else { ink(TEXT).bold() };
    let rank = if selected { format!("›{:>2}", index + 1) } else { format!("{:>3}", index + 1) };
    cv.text(row.x, row.y, &rank, ink(if selected { MAGENTA } else { MUTED }), 3);

    let value = |amount: f64| match (entry.measured, session) {
        (false, _) => "—".to_string(),
        (true, true) => bytes(amount),
        (true, false) => rate(amount),
    };
    if wide {
        let layout = Columns::new(row.w);
        cv.text(row.x + layout.name_x, row.y, &clip(&entry.label, layout.name_w), name_style, layout.name_w);
        cv.text(row.x + layout.hint_x, row.y, &clip(entry.hint(), layout.hint_w), ink(VIOLET), layout.hint_w);
        draw_meter(cv, row.x + layout.down_x, row.y, layout.meter_w, &value(entry.down), entry.down, scale, CYAN);
        draw_meter(cv, row.x + layout.up_x, row.y, layout.meter_w, &value(entry.up), entry.up, scale, MAGENTA);
    } else {
        let name_w = row.w.saturating_sub(4);
        let name = clip(&entry.label, name_w);
        let used = cv.text(row.x + 4, row.y, &name, name_style, name_w);
        let hint = fit(entry.hint(), name_w.saturating_sub(used + 2));
        cv.text(row.x + 4 + used + 2, row.y, &hint, ink(VIOLET), columns(&hint));
        if row.h > 1 {
            let half = row.w.saturating_sub(5) / 2;
            draw_meter(cv, row.x + 4, row.y + 1, half, &format!("↓ {}", value(entry.down)), entry.down, scale, CYAN);
            draw_meter(cv, row.x + 5 + half, row.y + 1, half, &format!("↑ {}", value(entry.up)), entry.up, scale, MAGENTA);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_meter(cv: &mut Canvas, x: usize, y: usize, w: usize, label: &str, value: f64, scale: f64, color: super::canvas::Color) {
    let label_w = 12.min(w / 2);
    cv.text(x, y, label, ink(color).bold(), label_w);
    let track = w.saturating_sub(label_w + 1);
    let fraction = if scale > 0.0 { value / scale } else { 0.0 };
    meter(cv, x + label_w + 1, y, track, fraction, fade(color, 0.5), color);
}
