//! The terminal dashboard: state, frame chrome, input and the main loop.
mod canvas;
mod intel;
mod theme;
mod top;
mod views;
mod widgets;

use crate::collector::{Collector, Snapshot};
use canvas::{clip, columns, Canvas, ColorMode, Rect};
use intel::Intel;
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    io::{self, BufReader, IsTerminal, Read, Write},
    os::raw::{c_int, c_ulong},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};
use theme::*;
use top::{Sort, Top};
use widgets::{chip, clock, keycap, panel, rate};

const POLL: Duration = Duration::from_millis(100);
const HISTORY: usize = 300;
const MOTION_FRAME: Duration = Duration::from_millis(110);
type Rates = (f64, f64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Overview,
    Links,
    Sockets,
    System,
    Flows,
    Analytics,
    Latency,
    Events,
}

/// View, full tab label and compact tab label.
const TABS: [(View, &str, &str); 8] = [
    (View::Overview, "Overview", "Home"),
    (View::Links, "Links", "Links"),
    (View::Sockets, "Sockets", "Socks"),
    (View::System, "System", "Sys"),
    (View::Flows, "Flows", "Flows"),
    (View::Analytics, "Analytics", "Stats"),
    (View::Latency, "Latency", "RTT"),
    (View::Events, "Events", "Log"),
];

impl View {
    fn index(self) -> usize {
        TABS.iter().position(|tab| tab.0 == self).unwrap_or(0)
    }

    /// Each view takes its hue from its place along the spectrum.
    fn accent(self) -> canvas::Color {
        spectrum(self.index() as f32 / TABS.len() as f32 * 0.66)
    }
}

enum Key {
    Quit,
    Pause,
    View(View),
    Scroll(isize),
    NextInterface(isize),
    Sort(Sort),
    Group,
    Period,
    Loopback,
    Inspect,
    Braille,
    Motion,
    Help,
    /// Any other key; it only dismisses the help panel.
    Other,
}

struct App {
    snapshot: Snapshot,
    histories: HashMap<String, VecDeque<Rates>>,
    aggregate: VecDeque<Rates>,
    sockets: VecDeque<f64>,
    selected: Option<String>,
    view: View,
    /// Scroll offset of the current view; clamped while rendering.
    scroll: Cell<usize>,
    /// First row, last row and total of the visible scroll window, for the footer.
    scroll_info: Cell<Option<(usize, usize, usize)>>,
    paused: bool,
    interval: Duration,
    top: Top,
    intel: Intel,
    braille: bool,
    motion: bool,
    help: bool,
    tick: u64,
    colors: ColorMode,
}

impl App {
    fn new(snapshot: Snapshot, interval: Duration, colors: ColorMode) -> Self {
        let mut app = Self {
            snapshot,
            histories: HashMap::new(),
            aggregate: VecDeque::new(),
            sockets: VecDeque::new(),
            selected: None,
            view: View::Overview,
            scroll: Cell::new(0),
            scroll_info: Cell::new(None),
            paused: false,
            interval,
            top: Top::default(),
            intel: Intel::default(),
            braille: true,
            motion: true,
            help: false,
            tick: 0,
            colors,
        };
        app.record();
        app
    }

    fn record(&mut self) {
        append_sample(&mut self.sockets, self.snapshot.connection_count as f64);
        self.histories.retain(|name, _| self.snapshot.interfaces.iter().any(|link| link.name == *name));
        for link in &self.snapshot.interfaces {
            append_sample(self.histories.entry(link.name.clone()).or_default(), (link.rx_rate, link.tx_rate));
        }
        if let Some(point) = self.snapshot.history.back() {
            append_sample(&mut self.aggregate, (point.rx, point.tx));
        }
        if self.selected.as_ref().is_some_and(|name| !self.histories.contains_key(name)) {
            self.selected = None;
        }
        self.intel.record(&self.snapshot);
        for (name, history) in &self.histories {
            if name != "lo" {
                self.intel.spike(name, history, self.snapshot.uptime);
            }
        }
    }

    fn in_scope(&self, name: &str) -> bool {
        match self.selected.as_deref() {
            Some(selected) => name == selected,
            None => name != "lo" || self.snapshot.interfaces.len() == 1,
        }
    }

    fn select_interface(&mut self, direction: isize) {
        let links = &self.snapshot.interfaces;
        let current = self.selected.as_ref()
            .and_then(|name| links.iter().position(|link| &link.name == name))
            .map_or(0, |index| index + 1);
        let next = (current as isize + direction).rem_euclid((links.len() + 1) as isize) as usize;
        self.selected = next.checked_sub(1).map(|index| links[index].name.clone());
    }

    /// The traffic series for the selected scope and its display name.
    fn traffic(&self) -> (&str, &VecDeque<Rates>) {
        if let Some((name, history)) = self.selected.as_deref().and_then(|name| self.histories.get(name).map(|history| (name, history))) {
            return (name, history);
        }
        if self.snapshot.interfaces.len() == 1 {
            if let Some(history) = self.histories.get("lo") {
                return ("lo (loopback)", history);
            }
        }
        ("all links", &self.aggregate)
    }

    fn scope_names(&self) -> Vec<&str> {
        self.snapshot.interfaces.iter().map(|link| link.name.as_str()).filter(|name| self.in_scope(name)).collect()
    }

    fn animating(&self) -> bool {
        self.motion && !self.paused && self.colors != ColorMode::Off
    }

    fn render(&self, w: usize, h: usize) -> Canvas {
        let mut cv = Canvas::new(w, h, BG, TEXT);
        if w < 32 || h < 10 {
            cv.text(0, 0, "MONET · resize terminal · q quits", ink(TEXT), w);
            return cv;
        }
        self.scroll_info.set(None);
        self.header(&mut cv);
        self.tabs(&mut cv);
        self.rule(&mut cv);
        let margin = usize::from(w >= 60);
        let body = Rect::new(margin, 3, w - 2 * margin, h - 4);
        match self.view {
            View::Overview => self.overview(&mut cv, body),
            View::Links => self.links(&mut cv, body),
            View::Sockets => self.sockets_view(&mut cv, body),
            View::System => self.system(&mut cv, body),
            View::Flows => self.scroll_info.set(self.top.render(&mut cv, body, &self.snapshot)),
            View::Analytics => self.analytics(&mut cv, body),
            View::Latency => self.latency(&mut cv, body),
            View::Events => self.events(&mut cv, body),
        }
        self.footer(&mut cv);
        if self.help {
            self.help_overlay(&mut cv);
        }
        cv
    }

    fn header(&self, cv: &mut Canvas) {
        let w = cv.w;
        // A faint violet glow on the left edge of the bar.
        for x in 0..w {
            let glow = 0.22 * (1.0 - x as f32 / w as f32).powi(2);
            cv.put(x, 0, ' ', ink(TEXT).on(SURFACE.mix(VIOLET, glow)));
        }
        let shimmer = if self.animating() { self.tick as f32 * 0.02 } else { 0.0 };
        let brand = "◆ MONET";
        for (index, ch) in brand.chars().enumerate() {
            let color = spectrum(index as f32 / 9.0 - shimmer);
            cv.put(1 + index, 0, ch, ink(color).bold());
        }
        let mut x = 1 + brand.chars().count() + 2;
        let host = if self.snapshot.hostname.is_empty() { "localhost" } else { &self.snapshot.hostname };

        // Right side: live state, interval and session time.
        let interval = format!("{}s", self.interval.as_secs_f64());
        let uptime = format!("up {}", clock(self.snapshot.uptime));
        let wide = w >= 70;
        let mut right = w - 1;
        if wide {
            right -= cv.text_right(right, 0, &uptime, ink(MUTED), 16) + 2;
            right -= cv.text_right(right, 0, &interval, ink(MUTED), 8) + 2;
        }
        if self.paused {
            let label = " PAUSED ";
            right -= columns(label);
            cv.text(right, 0, label, ink(SURFACE).on(AMBER).bold(), columns(label));
        } else {
            let pulse = if self.animating() { ((self.tick as f32 * 0.35).sin() + 1.0) / 2.0 } else { 1.0 };
            right -= cv.text_right(right, 0, "LIVE", ink(LIME).bold(), 4) + 1;
            right -= 1;
            cv.put(right, 0, '●', ink(LIME.mix(SURFACE, 0.6 * (1.0 - pulse))));
        }
        if x + 4 < right {
            x += cv.text(x, 0, "/ ", ink(LINE), 2);
            cv.text(x, 0, &clip(host, right.saturating_sub(x + 2)), ink(TEXT), right.saturating_sub(x + 2));
        }
    }

    fn tabs(&self, cv: &mut Canvas) {
        let w = cv.w;
        let full: usize = TABS.iter().map(|tab| columns(tab.1) + 5).sum::<usize>() + 1;
        let short: usize = TABS.iter().map(|tab| columns(tab.2) + 5).sum::<usize>() + 1;
        if short > w {
            let (view, label, _) = TABS[self.view.index()];
            let text = format!(" {} {label} ", view.index() + 1);
            let used = cv.text(1, 1, &text, ink(WHITE).on(RAISED).bold(), w - 1);
            cv.text(used + 2, 1, "1–8 views", ink(MUTED), w.saturating_sub(used + 2));
            return;
        }
        let use_full = full <= w;
        let mut x = 1;
        for (view, label, compact) in TABS {
            let label = if use_full { label } else { compact };
            let active = view == self.view;
            let number = format!(" {} ", view.index() + 1);
            let (number_style, label_style) = if active {
                (ink(view.accent()).on(RAISED).bold(), ink(WHITE).on(RAISED).bold())
            } else {
                (ink(LINE), ink(MUTED))
            };
            x += cv.text(x, 1, &number, number_style, w - x);
            x += cv.text(x, 1, &format!("{label} "), label_style, w - x);
            x += 1;
        }
        if use_full && w >= x + 9 {
            cv.text_right(w - 1, 1, "? help", ink(MUTED), 8);
        }
    }

    /// The rule under the tabs: lit beneath the active tab, with a comet of
    /// light sweeping across it when motion is on.
    fn rule(&self, cv: &mut Canvas) {
        let w = cv.w;
        let (start, end) = self.active_tab_span(w);
        let accent = self.view.accent();
        let comet = self.animating().then(|| (self.tick as f32 * 1.6) % (w as f32 + 40.0) - 20.0);
        for x in 0..w {
            let lit = x >= start && x < end;
            let mut color = if lit { accent } else { fade(LINE, 0.25) };
            if let Some(head) = comet {
                let behind = head - x as f32;
                if (0.0..18.0).contains(&behind) {
                    let strength = (1.0 - behind / 18.0).powi(2);
                    color = color.mix(glow(spectrum(x as f32 / w as f32)), strength);
                }
            }
            cv.put(x, 2, if lit { '━' } else { '─' }, ink(color));
        }
    }

    fn active_tab_span(&self, w: usize) -> (usize, usize) {
        let full: usize = TABS.iter().map(|tab| columns(tab.1) + 5).sum::<usize>() + 1;
        let short: usize = TABS.iter().map(|tab| columns(tab.2) + 5).sum::<usize>() + 1;
        if short > w {
            let label = TABS[self.view.index()].1;
            return (1, 1 + columns(label) + 4);
        }
        let mut x = 1;
        for (view, label, compact) in TABS {
            let width = columns(if full <= w { label } else { compact }) + 4;
            if view == self.view {
                return (x, x + width);
            }
            x += width + 1;
        }
        (0, 0)
    }

    fn footer(&self, cv: &mut Canvas) {
        let y = cv.h - 1;
        let w = cv.w;
        // (key, label, priority): the lowest priorities go first when space runs out.
        let mut keys: Vec<(&str, &str, u8)> = if self.view == View::Flows {
            vec![("d u t", "sort", 6), ("g", "group", 5), ("b", "live/session", 4), ("l", "loopback", 2), ("↑↓", "select", 3), ("enter", "inspect", 1), ("?", "help", 8), ("q", "quit", 9)]
        } else {
            vec![("1-8", "views", 7), ("[ ]", "link", 3), ("↑↓", "scroll", 4), ("c", "chart", 1), ("m", "motion", 2), ("p", "pause", 5), ("?", "help", 8), ("q", "quit", 9)]
        };
        let status = match self.scroll_info.get() {
            Some((first, last, total)) => format!("↕ {first}–{last} of {total}"),
            None => format!("◈ {}", self.traffic().0),
        };
        let status_w = columns(&status);
        let width = |keys: &[(&str, &str, u8)]| keys.iter().map(|(key, label, _)| columns(key) + columns(label) + 4).sum::<usize>();
        // The status keeps its place unless it would crowd out quit and help.
        let show_status = status_w + 3 <= w / 2;
        let room = if show_status { w.saturating_sub(status_w + 3) } else { w.saturating_sub(1) };
        while width(&keys) > room.saturating_sub(1) && !keys.is_empty() {
            let lowest = keys.iter().enumerate().min_by_key(|(_, key)| key.2).map_or(0, |(index, _)| index);
            keys.remove(lowest);
        }
        let mut x = 1;
        for (key, label, _) in &keys {
            x += keycap(cv, x, y, key, label, w.saturating_sub(x));
        }
        if show_status && x + status_w + 2 <= w {
            cv.text_right(w - 1, y, &status, ink(VIOLET), status_w);
        }
    }

    fn help_overlay(&self, cv: &mut Canvas) {
        let area = cv.area();
        cv.tint(Rect::new(0, 1, area.w, area.h - 1), BG, 0.6);
        let rows: &[(&str, &str)] = &[
            ("1 – 8", "switch view"),
            ("[  ]", "previous / next link for charts"),
            ("↑ ↓  j k", "scroll, or move the selection"),
            ("c", "braille or block charts"),
            ("m", "motion on or off"),
            ("p  space", "pause or resume sampling"),
            ("", ""),
            ("d u t", "flows: sort by down, up, total"),
            ("g", "flows: connection, peer, process"),
            ("b", "flows: live rate or session bytes"),
            ("l", "flows: include loopback"),
            ("enter", "flows: inspect the selection"),
            ("", ""),
            ("?", "close this panel"),
            ("q  ctrl-c", "quit"),
        ];
        let w = 54.min(area.w - 2);
        let h = (rows.len() + 4).min(area.h - 2);
        let modal = Rect::new((area.w - w) / 2, (area.h - h) / 2 + 1, w, h);
        let inner = panel(cv, modal, "Keys", "monet", VIOLET);
        for (index, (key, label)) in rows.iter().enumerate().take(inner.h.saturating_sub(1)) {
            let y = inner.y + 1 + index;
            if !key.is_empty() {
                chip(cv, inner.x, y, &format!("{key:<9}"), TEXT, RAISED, 12);
                cv.text(inner.x + 13, y, label, ink(MUTED), inner.w.saturating_sub(13));
            }
        }
    }
}

fn append_sample<T>(history: &mut VecDeque<T>, value: T) {
    if history.len() == HISTORY {
        history.pop_front();
    }
    history.push_back(value);
}

struct Terminal {
    saved_mode: String,
}

impl Terminal {
    fn enter() -> io::Result<Self> {
        let saved_mode = stty(&["-g"])?;
        let terminal = Self { saved_mode };
        // Ctrl+C arrives as a byte so the terminal is restored on quit.
        stty(&["-icanon", "-echo", "-isig", "min", "1", "time", "0"])?;
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[H")?;
        stdout.flush()?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = stty(&[&self.saved_mode]);
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(b"\x1b[0m\x1b[?7h\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
    }
}

fn stty(args: &[&str]) -> io::Result<String> {
    let output = Command::new("stty").args(args).stdin(Stdio::inherit()).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    } else {
        Err(io::Error::other(format!("could not configure terminal: {}", String::from_utf8_lossy(&output.stderr).trim())))
    }
}

#[repr(C)]
#[derive(Default)]
struct WinSize {
    rows: u16,
    columns: u16,
    x_pixels: u16,
    y_pixels: u16,
}

fn terminal_size() -> (usize, usize) {
    extern "C" {
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }
    #[cfg(target_os = "macos")]
    const TIOCGWINSZ: c_ulong = 0x40087468;
    // Linux uses the alternate ioctl encoding on these architectures.
    #[cfg(all(not(target_os = "macos"), any(target_arch = "mips", target_arch = "mips64", target_arch = "powerpc", target_arch = "powerpc64", target_arch = "sparc", target_arch = "sparc64")))]
    const TIOCGWINSZ: c_ulong = 0x40087468;
    #[cfg(all(not(target_os = "macos"), not(any(target_arch = "mips", target_arch = "mips64", target_arch = "powerpc", target_arch = "powerpc64", target_arch = "sparc", target_arch = "sparc64"))))]
    const TIOCGWINSZ: c_ulong = 0x5413;

    let mut size = WinSize::default();
    // SAFETY: TIOCGWINSZ writes a winsize struct to the supplied pointer.
    let result = unsafe { ioctl(1, TIOCGWINSZ, &mut size as *mut WinSize) };
    if result == 0 && size.columns > 1 && size.rows > 0 {
        // Leave the last column empty to avoid automatic line wrap.
        (usize::from(size.columns - 1), usize::from(size.rows))
    } else {
        (79, 24)
    }
}

fn input(sender: mpsc::Sender<Key>) {
    let mut escape = 0;
    for byte in BufReader::new(io::stdin().lock()).bytes() {
        let Ok(byte) = byte else { break };
        let key = match (escape, byte) {
            (_, b'\x03' | b'q' | b'Q') => Some(Key::Quit),
            (_, b'\x1b') => {
                escape = 1;
                continue;
            }
            (1, b'[' | b'O') => {
                escape = 2;
                continue;
            }
            (2, b'A') => Some(Key::Scroll(-1)),
            (2, b'B') => Some(Key::Scroll(1)),
            (2, b'5') => Some(Key::Scroll(-10)),
            (2, b'6') => Some(Key::Scroll(10)),
            (0, b'k') => Some(Key::Scroll(-1)),
            (0, b'j') => Some(Key::Scroll(1)),
            (0, b'[') => Some(Key::NextInterface(-1)),
            (0, b']') => Some(Key::NextInterface(1)),
            (0, b'p' | b' ') => Some(Key::Pause),
            (0, b'1'..=b'8') => Some(Key::View(TABS[usize::from(byte - b'1')].0)),
            (0, b'c') => Some(Key::Braille),
            (0, b'm') => Some(Key::Motion),
            (0, b'?' | b'h') => Some(Key::Help),
            (0, b'd') => Some(Key::Sort(Sort::Download)),
            (0, b'u') => Some(Key::Sort(Sort::Upload)),
            (0, b't') => Some(Key::Sort(Sort::Total)),
            (0, b'g') => Some(Key::Group),
            (0, b'b') => Some(Key::Period),
            (0, b'l') => Some(Key::Loopback),
            (0, b'\r' | b'\n') => Some(Key::Inspect),
            (0, _) => Some(Key::Other),
            _ => None,
        };
        escape = 0;
        if let Some(key) = key {
            let quit = matches!(key, Key::Quit);
            if sender.send(key).is_err() || quit {
                break;
            }
        }
    }
}

fn sample(collector: &mut Collector, interval_ms: u64) -> Snapshot {
    let mut snapshot = collector.sample();
    snapshot.interval_ms = interval_ms;
    snapshot
}

/// Plain text for redirected input or output.
fn report(snapshot: &Snapshot) {
    let (rx, tx) = snapshot.history.back().map_or((0.0, 0.0), |point| (point.rx, point.tx));
    println!("Monet · {} · {} sockets", clip(&snapshot.hostname, 128), snapshot.connection_count);
    println!("↓ {}   ↑ {}   (all links except loopback)", rate(rx), rate(tx));
    for link in &snapshot.interfaces {
        let state = if link.name == "lo" { "loopback" } else if link.state.is_empty() { "unknown" } else { &link.state };
        println!();
        println!("{}  ·  {state}", clip(&link.name, 64));
        println!("  ↓ {}   ↑ {}", rate(link.rx_rate), rate(link.tx_rate));
        println!("  total ↓ {}   ↑ {}", widgets::bytes(link.counters.rx as f64), widgets::bytes(link.counters.tx as f64));
        for address in &link.addresses {
            println!("  {}", clip(address, 64));
        }
    }
}

pub fn run(mut collector: Collector, interval_ms: u64, color: bool, motion: bool) -> io::Result<()> {
    let interval = Duration::from_millis(interval_ms);
    canvas::init_locale();

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        // Two readings, so the report contains measured rates.
        collector.sample();
        thread::sleep(interval);
        report(&sample(&mut collector, interval_ms));
        return Ok(());
    }

    let colors = ColorMode::detect(color);
    let _terminal = Terminal::enter()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || input(sender));
    let mut app = App::new(sample(&mut collector, interval_ms), interval, colors);
    app.motion = motion;
    let mut next_sample = Instant::now() + interval;
    let mut next_frame = Instant::now() + MOTION_FRAME;
    let mut size = terminal_size();
    let mut dirty = true;
    // Set when only the animated chrome (header, tabs, rule) changed.
    let mut motion_only = false;
    // Declared after the terminal guard so stdout unlocks before restoration.
    let mut stdout = io::stdout().lock();

    loop {
        let new_size = terminal_size();
        if new_size != size {
            size = new_size;
            dirty = true;
        }
        if !app.paused && Instant::now() >= next_sample {
            app.snapshot = sample(&mut collector, interval_ms);
            app.record();
            next_sample = Instant::now() + interval;
            dirty = true;
        }
        if app.animating() && Instant::now() >= next_frame {
            app.tick = app.tick.wrapping_add(1);
            next_frame = Instant::now() + MOTION_FRAME;
            motion_only = !dirty;
            dirty = true;
        }
        if dirty {
            let canvas = app.render(size.0, size.1);
            // Motion ticks repaint only the top rows; everything below is unchanged.
            let frame = if motion_only && !app.help { canvas.encode_top(colors, 3) } else { canvas.encode(colors) };
            motion_only = false;
            // Synchronized output keeps fast terminals from showing half a frame.
            stdout.write_all(b"\x1b[?2026h\x1b[H")?;
            stdout.write_all(frame.as_bytes())?;
            stdout.write_all(b"\x1b[?2026l")?;
            stdout.flush()?;
            dirty = false;
        }

        let mut timeout = POLL;
        if !app.paused {
            timeout = timeout.min(next_sample.saturating_duration_since(Instant::now()));
        }
        if app.animating() {
            timeout = timeout.min(next_frame.saturating_duration_since(Instant::now()));
        }
        let key = match receiver.recv_timeout(timeout) {
            Ok(Key::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(key) => key,
            Err(RecvTimeoutError::Timeout) => continue,
        };
        dirty = true;
        if app.help && !matches!(key, Key::Help) {
            app.help = false;
            continue;
        }
        match key {
            Key::Quit => break,
            Key::Help => app.help = !app.help,
            Key::Pause => {
                app.paused = !app.paused;
                if !app.paused {
                    // Reset the rate baseline so the pause does not distort
                    // the first sample after resuming.
                    collector.reset_flow_baseline();
                    let baseline = collector.sample();
                    app.intel.rebase(&baseline);
                    next_sample = Instant::now() + interval;
                }
            }
            Key::View(view) => {
                app.view = view;
                app.scroll.set(0);
            }
            Key::Scroll(direction) => {
                if app.view == View::Flows {
                    app.top.scroll(direction);
                } else {
                    app.scroll.set(app.scroll.get().saturating_add_signed(direction));
                }
            }
            Key::NextInterface(direction) => {
                app.select_interface(direction);
                if !matches!(app.view, View::Analytics) {
                    app.view = View::Overview;
                }
            }
            Key::Braille => app.braille = !app.braille,
            Key::Motion => app.motion = !app.motion,
            Key::Sort(sort) if app.view == View::Flows => app.top.sort(sort),
            Key::Group if app.view == View::Flows => app.top.group(),
            Key::Period if app.view == View::Flows => app.top.period(),
            Key::Loopback if app.view == View::Flows => app.top.loopback(),
            Key::Inspect if app.view == View::Flows => app.top.inspect(&app.snapshot),
            _ => dirty = false,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locale() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(canvas::init_locale);
    }

    fn assert_fills(app: &App, w: usize, h: usize) {
        let canvas = app.render(w, h);
        let rows = canvas.plain_rows();
        assert_eq!(rows.len(), h, "{:?} at {w}x{h}", app.view);
        assert!(rows.iter().all(|row| columns(row) == w), "{:?} ragged at {w}x{h}", app.view);
    }

    /// Every width and height around the layout thresholds, the Flows detail
    /// and modes, and the state where TCP diagnostics are unavailable.
    #[test]
    fn layouts_hold_at_every_threshold_and_in_every_state() {
        locale();
        let mut app = warmed_up();
        let mut sizes = Vec::new();
        for w in 32..=150 {
            sizes.extend([10, 12, 16, 19, 20, 22, 24, 30].map(|h| (w, h)));
        }
        for h in 10..=45 {
            sizes.extend([32, 40, 59, 60, 61, 76, 77, 78, 85, 114, 120].map(|w| (w, h)));
        }
        for blocked in [false, true] {
            if blocked {
                app.snapshot.traffic.error = Some("blocked".into());
                app.snapshot.traffic.entries.clear();
            }
            for (view, ..) in TABS {
                app.view = view;
                for &(w, h) in &sizes {
                    assert_fills(&app, w, h);
                }
            }
            app.view = View::Flows;
            for change in 0..4 {
                match change {
                    1 => app.top.inspect(&app.snapshot),
                    2 => { app.top.inspect(&app.snapshot); app.top.period(); }
                    3 => { app.top.group(); app.top.inspect(&app.snapshot); }
                    _ => {}
                }
                for &(w, h) in sizes.iter().step_by(7) {
                    assert_fills(&app, w, h);
                }
            }
        }
    }

    fn warmed_up() -> App {
        let mut collector = Collector::new();
        collector.sample();
        let mut app = App::new(collector.sample(), Duration::from_secs(1), ColorMode::Off);
        for _ in 0..3 {
            app.snapshot = collector.sample();
            app.record();
        }
        app
    }

    #[test]
    fn every_view_fills_small_and_wide_terminals_exactly() {
        locale();
        let mut app = warmed_up();
        for (view, step) in TABS.iter().map(|tab| tab.0).zip([0, 1, -1, 2, 0, 1, 3, -2]) {
            app.select_interface(step);
            for (w, h) in [(31, 9), (32, 10), (35, 12), (50, 14), (63, 18), (73, 20), (79, 24), (120, 40), (200, 60)] {
                for (braille, help) in [(true, false), (false, true)] {
                    app.view = view;
                    app.braille = braille;
                    app.help = help;
                    app.scroll.set(usize::MAX / 2);
                    let canvas = app.render(w, h);
                    let rows = canvas.plain_rows();
                    assert_eq!(rows.len(), h, "{view:?} at {w}x{h}");
                    assert!(rows.iter().all(|row| columns(row) == w), "{view:?} ragged at {w}x{h}");
                    let frame = canvas.encode(ColorMode::Off);
                    assert_eq!(frame.matches("\x1b[K\r\n").count(), h - 1);
                    assert!(frame.ends_with("\x1b[K\x1b[J"));
                }
            }
        }
    }

    #[test]
    fn color_frames_use_each_palette() {
        let app = warmed_up();
        let canvas = app.render(100, 30);
        assert!(canvas.encode(ColorMode::TrueColor).contains(";38;2;"));
        assert!(canvas.encode(ColorMode::Indexed).contains(";48;5;"));
        assert!(canvas.encode(ColorMode::Basic).contains(";40m"));
        assert!(!canvas.encode(ColorMode::Off).contains("\x1b[0"));
    }

    /// Print every view for a visual check:
    /// `cargo test --offline preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn preview() {
        locale();
        let mut app = warmed_up();
        let mut collector = Collector::new();
        collector.sample();
        let samples: usize = std::env::var("MONET_PREVIEW_SAMPLES").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
        app.help = std::env::var_os("MONET_PREVIEW_HELP").is_some();
        if std::env::var_os("MONET_PREVIEW_DETAIL").is_some() {
            app.top.inspect(&app.snapshot);
        }
        app.braille = std::env::var_os("MONET_PREVIEW_BLOCKS").is_none();
        for _ in 0..samples {
            std::thread::sleep(Duration::from_millis(250));
            app.snapshot = collector.sample();
            app.record();
        }
        let size = std::env::var("MONET_PREVIEW").ok()
            .and_then(|value| value.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))));
        let sizes = size.map_or_else(|| vec![(80, 24), (120, 40), (200, 60)], |size| vec![size]);
        for (view, ..) in TABS {
            for &(w, h) in &sizes {
                app.view = view;
                println!("── {view:?} {w}x{h}");
                let canvas = app.render(w, h);
                if std::env::var_os("MONET_PREVIEW_COLOR").is_some() {
                    println!("{}", canvas.encode(ColorMode::TrueColor));
                    continue;
                }
                for row in canvas.plain_rows() {
                    println!("{row}|");
                }
            }
        }
    }
}
