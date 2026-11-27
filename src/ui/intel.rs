//! History the dashboard builds from successive snapshots: per-link packet
//! and fault series, session volume, TCP round-trip and retransmit history,
//! and the event log.
use super::{append_sample, widgets::rate, Rates};
use crate::{collector::Snapshot, flows::peer_ip};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    net::SocketAddr,
};

const EVENT_LIMIT: usize = 500;
/// Keep one busy sample from pushing everything else out of the log.
const EVENTS_PER_SAMPLE: usize = 20;
/// Seconds between fault events for one interface; virtual bridges drop often.
const FAULT_QUIET: u64 = 60;
const SPIKE_FLOOR: f64 = 1024.0 * 1024.0;

#[derive(Default)]
pub struct Link {
    pub packets: VecDeque<Rates>,
    pub faults: VecDeque<f64>,
    state: String,
    /// Bytes counted since Monet started.
    pub moved: (u64, u64),
    last: (u64, u64, u64),
    spiking: bool,
    unreported: u64,
    reported_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Connect,
    Listen,
    Link,
    Fault,
    Spike,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Connect, Kind::Listen, Kind::Link, Kind::Fault, Kind::Spike];

    pub fn label(self) -> &'static str {
        match self {
            Self::Connect => "CONNECT",
            Self::Listen => "LISTEN",
            Self::Link => "LINK",
            Self::Fault => "FAULT",
            Self::Spike => "SPIKE",
        }
    }
}

pub struct Event {
    pub uptime: u64,
    pub kind: Kind,
    pub text: String,
}

#[derive(Default)]
pub struct Intel {
    pub links: BTreeMap<String, Link>,
    pub rtt_ms: VecDeque<f64>,
    pub retransmits: VecDeque<f64>,
    last_retransmits: Option<u64>,
    peers: HashSet<String>,
    listeners: HashSet<String>,
    pub events: VecDeque<Event>,
    started: bool,
}

impl Intel {
    /// Forget counter baselines so a pause does not appear as one huge sample.
    pub fn rebase(&mut self, snapshot: &Snapshot) {
        for iface in &snapshot.interfaces {
            if let Some(link) = self.links.get_mut(&iface.name) {
                link.last = counters(iface);
            }
        }
        self.last_retransmits = snapshot.stats.get("Tcp.RetransSegs").copied();
    }

    pub fn record(&mut self, snapshot: &Snapshot) {
        let mut events = Vec::new();
        self.record_links(snapshot, &mut events);
        self.record_sockets(snapshot, &mut events);

        let rtts = measured_rtts(snapshot);
        let mean = rtts.iter().sum::<f64>() / rtts.len().max(1) as f64;
        append_sample(&mut self.rtt_ms, mean);

        let total = snapshot.stats.get("Tcp.RetransSegs").copied();
        let delta = match (total, self.last_retransmits) {
            (Some(now), Some(before)) => now.saturating_sub(before),
            _ => 0,
        };
        self.last_retransmits = total;
        append_sample(&mut self.retransmits, delta as f64);

        for (kind, text) in events {
            self.push(Event { uptime: snapshot.uptime, kind, text });
        }
        self.started = true;
    }

    fn push(&mut self, event: Event) {
        if self.events.len() == EVENT_LIMIT {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn record_links(&mut self, snapshot: &Snapshot, events: &mut Vec<(Kind, String)>) {
        let started = self.started;
        self.links.retain(|name, _| {
            let present = snapshot.interfaces.iter().any(|iface| iface.name == *name);
            if !present {
                events.push((Kind::Link, format!("{name} removed")));
            }
            present
        });
        for iface in &snapshot.interfaces {
            let now = counters(iface);
            let new = !self.links.contains_key(&iface.name);
            let link = self.links.entry(iface.name.clone()).or_default();
            if new {
                link.last = now;
                link.state.clone_from(&iface.state);
                if started {
                    events.push((Kind::Link, format!("{} appeared", iface.name)));
                }
            }
            if link.state != iface.state {
                events.push((Kind::Link, format!("{} {} → {}", iface.name, link.state, iface.state)));
                link.state.clone_from(&iface.state);
            }
            link.moved.0 = link.moved.0.saturating_add(now.0.saturating_sub(link.last.0));
            link.moved.1 = link.moved.1.saturating_add(now.1.saturating_sub(link.last.1));
            let faults = now.2.saturating_sub(link.last.2);
            link.unreported = link.unreported.saturating_add(faults);
            let due = link.reported_at.map_or(true, |at| snapshot.uptime >= at + FAULT_QUIET);
            if link.unreported > 0 && due {
                events.push((Kind::Fault, format!("{} +{} errors/drops", iface.name, link.unreported)));
                link.unreported = 0;
                link.reported_at = Some(snapshot.uptime);
            }
            link.last = now;
            append_sample(&mut link.faults, faults as f64);
            append_sample(&mut link.packets, (iface.rx_pps, iface.tx_pps));
        }
    }

    fn record_sockets(&mut self, snapshot: &Snapshot, events: &mut Vec<(Kind, String)>) {
        // A truncated table would make sockets seem to come and go.
        if snapshot.connection_count > snapshot.connections.len() {
            return;
        }
        let mut peers = HashSet::new();
        let mut listeners = HashSet::new();
        let (mut connects, mut listens) = (0, 0);
        for socket in &snapshot.connections {
            let owner = socket.process.as_deref().unwrap_or("?");
            match socket.state.as_str() {
                "LISTEN" => {
                    let key = format!("{} {}", socket.protocol, socket.local);
                    if self.started && !self.listeners.contains(&key) && !listeners.contains(&key) && listens < EVENTS_PER_SAMPLE {
                        events.push((Kind::Listen, format!("{key}  [{owner}]")));
                        listens += 1;
                    }
                    listeners.insert(key);
                }
                "ESTABLISHED" => {
                    let Ok(remote) = socket.remote.parse::<SocketAddr>() else { continue };
                    if peer_ip(remote.ip()).is_loopback() {
                        continue;
                    }
                    let key = socket.remote.clone();
                    if self.started && !self.peers.contains(&key) && !peers.contains(&key) && connects < EVENTS_PER_SAMPLE {
                        events.push((Kind::Connect, format!("{key}  [{owner}]")));
                        connects += 1;
                    }
                    peers.insert(key);
                }
                _ => {}
            }
        }
        self.peers = peers;
        self.listeners = listeners;
    }

    /// Log a sudden jump well above an interface's recent average.
    pub fn spike(&mut self, name: &str, history: &VecDeque<Rates>, uptime: u64) {
        let Some(link) = self.links.get_mut(name) else { return };
        let Some(&(rx, tx)) = history.back() else { return };
        let before: Vec<f64> = history.iter().rev().skip(1).take(30).map(|&(rx, tx)| rx + tx).collect();
        let spiking = is_spike(rx + tx, &before);
        let fire = spiking && !link.spiking;
        link.spiking = spiking;
        if fire {
            let text = format!("{name} ↓ {}  ↑ {}", rate(rx), rate(tx));
            self.push(Event { uptime, kind: Kind::Spike, text });
        }
    }

    /// Sum a per-interface series, aligned on the newest sample.
    pub fn combined(&self, scope: &[&str], pick: impl Fn(&Link) -> Vec<f64>) -> Vec<f64> {
        let mut total: Vec<f64> = Vec::new();
        for link in scope.iter().filter_map(|name| self.links.get(*name)) {
            let mut series = pick(link);
            if series.len() > total.len() {
                std::mem::swap(&mut series, &mut total);
            }
            let offset = total.len() - series.len();
            for (index, value) in series.into_iter().enumerate() {
                total[offset + index] += value;
            }
        }
        total
    }
}

fn counters(iface: &crate::collector::Interface) -> (u64, u64, u64) {
    let c = &iface.counters;
    let faults = c.rx_errors.saturating_add(c.tx_errors).saturating_add(c.rx_drops).saturating_add(c.tx_drops);
    (c.rx, c.tx, faults)
}

fn is_spike(current: f64, before: &[f64]) -> bool {
    if before.len() < 10 || current < SPIKE_FLOOR {
        return false;
    }
    current > 4.0 * before.iter().sum::<f64>() / before.len() as f64
}

pub fn measured_rtts(snapshot: &Snapshot) -> Vec<f64> {
    if snapshot.traffic.error.is_some() {
        return Vec::new();
    }
    snapshot.traffic.entries.iter()
        .filter(|flow| flow.active && flow.rtt_us > 0 && !flow.loopback())
        .map(|flow| f64::from(flow.rtt_us) / 1000.0)
        .collect()
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub now: f64,
    pub average: f64,
    pub p95: f64,
    pub peak: f64,
}

pub fn summary(values: &[f64]) -> Summary {
    let Some(&now) = values.last() else { return Summary::default() };
    let mut sorted = values.to_vec();
    sorted.sort_unstable_by(f64::total_cmp);
    let rank = (sorted.len() * 95).div_ceil(100).max(1) - 1;
    Summary {
        now,
        average: sorted.iter().sum::<f64>() / sorted.len() as f64,
        p95: sorted[rank],
        peak: sorted[sorted.len() - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_report_average_percentile_and_peak() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summary(&values);
        assert_eq!((s.now, s.average, s.p95, s.peak), (100.0, 50.5, 95.0, 100.0));
        assert_eq!(summary(&[]), Summary::default());
    }

    #[test]
    fn spikes_need_history_volume_and_a_real_jump() {
        let quiet = [1000.0; 20];
        assert!(is_spike(8.0 * SPIKE_FLOOR, &quiet));
        assert!(!is_spike(SPIKE_FLOOR / 2.0, &quiet));
        assert!(!is_spike(8.0 * SPIKE_FLOOR, &quiet[..5]));
        assert!(!is_spike(8.0 * SPIKE_FLOOR, &[4.0 * SPIKE_FLOOR; 20]));
    }
}
