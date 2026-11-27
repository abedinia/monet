use crate::{
    collector::Connection,
    inet_diag::{self, TcpSample},
};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    time::Instant,
};

const CLOSED_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Default)]
pub struct Transfer {
    pub download: u64,
    pub upload: u64,
}

impl Transfer {
    fn delta(self, previous: Self) -> Option<Self> {
        Some(Self {
            download: self.download.checked_sub(previous.download)?,
            upload: self.upload.checked_sub(previous.upload)?,
        })
    }

    fn add(&mut self, other: Self) {
        self.download = self.download.saturating_add(other.download);
        self.upload = self.upload.saturating_add(other.upload);
    }
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub cookie: u64,
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub inode: u64,
    pub process: Option<String>,
    pub pid: Option<u32>,
    pub state: u8,
    pub session: Transfer,
    pub lifetime: Transfer,
    pub rate: Option<(f64, f64)>,
    pub active: bool,
    pub rtt_us: u32,
    pub retransmits: u32,
    last_seen: Instant,
}

impl Flow {
    pub fn loopback(&self) -> bool {
        peer_ip(self.remote.ip()).is_loopback()
    }

    pub fn state_label(&self) -> &'static str {
        match self.state {
            1 => "ESTABLISHED",
            2 => "SYN_SENT",
            3 => "SYN_RECV",
            4 => "FIN_WAIT1",
            5 => "FIN_WAIT2",
            7 => "CLOSED",
            8 => "CLOSE_WAIT",
            9 => "LAST_ACK",
            11 => "CLOSING",
            _ => "OTHER",
        }
    }
}

pub fn peer_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}

#[derive(Default)]
pub struct FlowSnapshot {
    pub entries: Vec<Flow>,
    pub error: Option<String>,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct FlowId {
    cookie: u64,
    inode: u64,
    local: SocketAddr,
    remote: SocketAddr,
}

impl From<TcpSample> for FlowId {
    fn from(sample: TcpSample) -> Self {
        Self { cookie: sample.cookie, inode: sample.inode, local: sample.local, remote: sample.remote }
    }
}

#[derive(Default)]
pub struct Tracker {
    tracked: HashMap<FlowId, Flow>,
    previous_sample: Option<Instant>,
}

impl Tracker {
    pub fn reset_baseline(&mut self) {
        self.previous_sample = None;
        for flow in self.tracked.values_mut() {
            flow.rate = None;
        }
    }

    pub fn sample(&mut self, connections: &[Connection]) -> FlowSnapshot {
        let now = Instant::now();
        let error = match inet_diag::collect() {
            Ok(samples) => {
                self.update(samples, connections, now);
                None
            }
            Err(error) => {
                self.reset_baseline();
                Some(error.to_string())
            }
        };
        FlowSnapshot { entries: self.tracked.values().cloned().collect(), error }
    }

    fn update(&mut self, samples: Vec<TcpSample>, connections: &[Connection], now: Instant) {
        let owners: HashMap<_, _> = connections.iter()
            .filter(|row| row.inode != 0 && row.process.is_some())
            .map(|row| (row.inode, row))
            .collect();
        let previous = self.previous_sample.replace(now);
        let elapsed = previous.map(|then| now.duration_since(then).as_secs_f64().max(0.001));

        let mut seen = HashSet::with_capacity(samples.len());
        for sample in samples {
            let id = FlowId::from(sample);
            if !seen.insert(id) {
                continue;
            }
            let lifetime = Transfer { download: sample.download, upload: sample.upload_acked };
            let flow = self.tracked.entry(id).or_insert_with(|| Flow {
                cookie: sample.cookie,
                local: sample.local, remote: sample.remote, inode: sample.inode,
                process: None, pid: None, state: sample.state,
                session: Transfer::default(), lifetime, rate: None,
                active: false, rtt_us: 0, retransmits: 0, last_seen: now,
            });

            flow.rate = None;
            if flow.active && previous == Some(flow.last_seen) {
                if let (Some(delta), Some(elapsed)) = (lifetime.delta(flow.lifetime), elapsed) {
                    flow.session.add(delta);
                    flow.rate = Some((delta.download as f64 / elapsed, delta.upload as f64 / elapsed));
                }
            }

            if let Some(owner) = owners.get(&sample.inode).filter(|row| {
                row.local.parse::<SocketAddr>().ok() == Some(sample.local)
                    && row.remote.parse::<SocketAddr>().ok() == Some(sample.remote)
            }) {
                flow.process.clone_from(&owner.process);
                flow.pid = owner.pid;
            }
            flow.lifetime = lifetime;
            flow.last_seen = now;
            flow.active = true;
            flow.state = sample.state;
            flow.rtt_us = sample.rtt_us;
            flow.retransmits = sample.retransmits;
        }

        self.tracked.retain(|id, flow| {
            if seen.contains(id) {
                return true;
            }
            flow.active = false;
            flow.rate = None;
            flow.session.download != 0 || flow.session.upload != 0
        });

        let closed_count = self.tracked.len().saturating_sub(seen.len());
        if closed_count > CLOSED_LIMIT {
            let mut closed: Vec<_> = self.tracked.iter()
                .filter(|(_, flow)| !flow.active)
                .map(|(&id, flow)| (id, flow.last_seen))
                .collect();
            let remove = closed_count - CLOSED_LIMIT;
            closed.select_nth_unstable_by_key(remove - 1, |entry| entry.1);
            for (id, _) in closed.into_iter().take(remove) {
                self.tracked.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample(cookie: u64, download: u64, upload: u64) -> TcpSample {
        TcpSample {
            cookie, inode: 12,
            local: "192.0.2.15:50330".parse().unwrap(),
            remote: "198.51.100.20:443".parse().unwrap(),
            state: 1, download, upload_acked: upload, rtt_us: 1200, retransmits: 1,
        }
    }

    #[test]
    fn deltas_use_elapsed_time_and_closed_flows_keep_session_bytes() {
        let now = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(vec![sample(1, 800, 90)], &[], now);
        let id = FlowId::from(sample(1, 0, 0));
        assert!(tracker.tracked[&id].rate.is_none());
        tracker.update(vec![sample(1, 1200, 290)], &[], now + Duration::from_secs(2));
        assert_eq!(tracker.tracked[&id].rate, Some((200.0, 100.0)));
        tracker.update(vec![sample(2, 9999, 9999)], &[], now + Duration::from_secs(3));
        let closed = &tracker.tracked[&id];
        assert!(!closed.active);
        assert_eq!((closed.session.download, closed.session.upload), (400, 200));
        assert!(tracker.tracked[&FlowId::from(sample(2, 0, 0))].rate.is_none());
    }

    #[test]
    fn a_sampling_gap_or_counter_reset_does_not_create_a_spike() {
        let now = Instant::now();
        let mut tracker = Tracker::default();
        tracker.update(vec![sample(1, 800, 90)], &[], now);
        tracker.reset_baseline();
        tracker.update(vec![sample(1, 80_000, 999)], &[], now + Duration::from_secs(5));
        let id = FlowId::from(sample(1, 0, 0));
        assert!(tracker.tracked[&id].rate.is_none());
        tracker.update(vec![sample(1, 120, 30)], &[], now + Duration::from_secs(6));
        assert!(tracker.tracked[&id].rate.is_none());
        assert_eq!(tracker.tracked[&id].session.download, 0);
    }
}
