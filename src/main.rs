mod collector;
mod flows;
mod inet_diag;
mod json;
mod ui;

use std::{env, process, thread, time::Duration};

const HELP: &str = "Monet — a terminal network monitor

Usage: monet [OPTIONS]

  --interval MS     Sampling interval, 250–10000 ms (default: 1000)
  --once            Print one JSON snapshot and exit
  --no-color        Disable ANSI colors
  --no-motion       Start with animation off (alias: --no-rain)
  --help, -h        Show this help
  --version, -V     Show version

Views: 1 overview, 2 links, 3 sockets, 4 system, 5 flows,
       6 analytics, 7 latency, 8 events.
Keys: ↑/↓ or j/k scroll, [/] link, c chart style, m motion, p pause, ? help, q quit.
Flows (5): d/u/t sort, g group, b live/session, l loopback, Enter inspect.
Linux and macOS. Run as your regular user.
";

fn run() -> Result<(), String> {
    let mut interval = 1000u64;
    let mut once = false;
    let mut color = env::var_os("NO_COLOR").is_none();
    let mut motion = true;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => { print!("{HELP}"); return Ok(()); }
            "--version" | "-V" => { println!("monet {}", env!("CARGO_PKG_VERSION")); return Ok(()); }
            "--interval" => {
                interval = args.next().ok_or("--interval requires milliseconds")?.parse().map_err(|_| "Invalid interval")?;
                if !(250..=10000).contains(&interval) { return Err("Interval must be 250–10000 ms".into()); }
            }
            "--terminal" => {},
            "--once" => once = true,
            "--no-color" => color = false,
            "--no-rain" | "--no-motion" => motion = false,
            _ => return Err(format!("Unknown option: {arg}. Use --help.")),
        }
    }
    let mut collector = collector::Collector::new();
    if once {
        collector.sample();
        thread::sleep(Duration::from_millis(interval));
        let mut snapshot = collector.sample();
        snapshot.interval_ms = interval;
        println!("{}", snapshot.to_json());
        return Ok(());
    }
    ui::run(collector, interval, color, motion).map_err(|error| error.to_string())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Monet: {error}");
        process::exit(1);
    }
}
