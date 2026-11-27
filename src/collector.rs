use crate::{flows::{Flow, FlowSnapshot, Tracker}, json::{array, object, quote}};
use std::{collections::{BTreeMap, HashMap, VecDeque}, fs, net::{Ipv4Addr, Ipv6Addr}, path::Path, time::{Instant, SystemTime, UNIX_EPOCH}};
#[cfg(target_os = "macos")]
use std::process::Command;

#[derive(Clone, Default, Debug)]
pub struct Counters {
    pub rx: u64, pub tx: u64, pub rx_packets: u64, pub tx_packets: u64,
    pub rx_errors: u64, pub tx_errors: u64, pub rx_drops: u64, pub tx_drops: u64,
}

#[derive(Clone, Debug)]
pub struct Interface {
    pub name: String, pub state: String, pub mac: String, pub mtu: String,
    pub speed_mbps: Option<u64>, pub addresses: Vec<String>, pub counters: Counters,
    pub rx_rate: f64, pub tx_rate: f64, pub rx_pps: f64, pub tx_pps: f64,
}

#[derive(Clone, Debug)]
pub struct Connection {
    pub protocol: String, pub local: String, pub remote: String, pub state: String,
    pub uid: u32, pub inode: u64, pub tx_queue: u64, pub rx_queue: u64,
    pub process: Option<String>, pub pid: Option<u32>,
}

#[derive(Clone, Default)]
pub struct SocketCounts {
    pub tcp: usize,
    pub udp: usize,
    pub established: usize,
    pub listening: usize,
}

pub struct Snapshot {
    pub timestamp: u64, pub uptime: u64, pub interval_ms: u64, pub hostname: String,
    pub interfaces: Vec<Interface>, pub connections: Vec<Connection>,
    pub routes: Vec<(String, String, String, u64)>, pub dns: Vec<String>,
    pub stats: BTreeMap<String, u64>, pub warnings: Vec<String>, pub history: VecDeque<Point>,
    pub connection_count: usize,
    pub sockets: SocketCounts,
    pub traffic: FlowSnapshot,
}

#[derive(Clone)]
pub struct Point { pub timestamp: u64, pub rx: f64, pub tx: f64 }

fn num(value: f64) -> String { if value.is_finite() { format!("{value:.2}") } else { "0".into() } }
fn counter_rate(current: u64, previous: u64, elapsed: f64) -> f64 {
    current.saturating_sub(previous) as f64 / elapsed.max(0.001)
}
fn s(key: &str, value: &str) -> (String, String) { (key.into(), quote(value)) }
fn n(key: &str, value: impl ToString) -> (String, String) { (key.into(), value.to_string()) }

impl Snapshot {
    pub fn to_json(&self) -> String {
        object(vec![
            n("timestamp", self.timestamp), n("uptime", self.uptime), n("interval_ms", self.interval_ms),
            s("hostname", &self.hostname), n("connection_count", self.connection_count),
            ("interfaces".into(), array(self.interfaces.iter().map(|i| object(vec![
                s("name", &i.name), s("state", &i.state), s("mac", &i.mac), s("mtu", &i.mtu),
                n("speed_mbps", i.speed_mbps.map(|v| v.to_string()).unwrap_or("null".into())),
                ("addresses".into(), array(i.addresses.iter().map(|v| quote(v)))),
                n("rx_bytes", i.counters.rx), n("tx_bytes", i.counters.tx),
                n("rx_packets", i.counters.rx_packets), n("tx_packets", i.counters.tx_packets),
                n("rx_errors", i.counters.rx_errors), n("tx_errors", i.counters.tx_errors),
                n("rx_drops", i.counters.rx_drops), n("tx_drops", i.counters.tx_drops),
                n("rx_rate", num(i.rx_rate)), n("tx_rate", num(i.tx_rate)),
                n("rx_pps", num(i.rx_pps)), n("tx_pps", num(i.tx_pps)),
            ])))),
            ("connections".into(), array(self.connections.iter().map(|c| object(vec![
                s("protocol", &c.protocol), s("local", &c.local), s("remote", &c.remote), s("state", &c.state),
                n("uid", c.uid), n("inode", c.inode), n("tx_queue", c.tx_queue), n("rx_queue", c.rx_queue),
                n("pid", c.pid.map(|p| p.to_string()).unwrap_or("null".into())),
                ("process".into(), c.process.as_deref().map(quote).unwrap_or("null".into())),
            ])))),
            ("routes".into(), array(self.routes.iter().map(|(iface, dst, gw, metric)| object(vec![
                s("interface", iface), s("destination", dst), s("gateway", gw), n("metric", metric),
            ])))),
            ("dns".into(), array(self.dns.iter().map(|d| quote(d)))),
            ("stats".into(), object(self.stats.iter().map(|(k,v)| (k.clone(), v.to_string())))),
            ("warnings".into(), array(self.warnings.iter().map(|w| quote(w)))),
            ("history".into(), array(self.history.iter().map(|p| object(vec![n("timestamp", p.timestamp), n("rx", num(p.rx)), n("tx", num(p.tx))])))),
            ("tcp_traffic".into(), object([
                ("error".into(), self.traffic.error.as_deref().map_or_else(|| "null".into(), quote)),
                ("flows".into(), array(self.traffic.entries.iter().map(flow_json))),
            ])),
        ])
    }
}

pub struct Collector {
    previous: HashMap<String, Counters>, last: Instant, start: Instant,
    history: VecDeque<Point>, owners: HashMap<u64, (u32, String)>, last_owners: Instant,
    tracker: Tracker,
}

impl Collector {
    pub fn new() -> Self {
        Self { previous: HashMap::new(), last: Instant::now(), start: Instant::now(),
            history: VecDeque::new(), owners: HashMap::new(), last_owners: Instant::now() - std::time::Duration::from_secs(10),
            tracker: Tracker::default() }
    }

    pub fn reset_flow_baseline(&mut self) { self.tracker.reset_baseline(); }

    pub fn sample(&mut self) -> Snapshot {
        #[cfg(target_os = "macos")]
        return self.sample_macos();

        #[cfg(not(target_os = "macos"))]
        {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64().max(0.001);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let mut warnings = Vec::new();
        let raw = fs::read_to_string("/proc/net/dev").unwrap_or_else(|e| { warnings.push(format!("Interface counters unavailable: {e}")); String::new() });
        let counters = parse_dev(&raw);
        let mut interfaces = Vec::new();
        let addresses = interface_addresses();
        for (name, counters) in counters {
            let root = format!("/sys/class/net/{name}");
            let old = self.previous.get(&name);
            let rate = |new: u64, old: u64| counter_rate(new, old, elapsed);
            let (rx_rate, tx_rate, rx_pps, tx_pps) = match old {
                Some(o) => (rate(counters.rx, o.rx), rate(counters.tx, o.tx), rate(counters.rx_packets, o.rx_packets), rate(counters.tx_packets, o.tx_packets)),
                None => (0.0, 0.0, 0.0, 0.0),
            };
            interfaces.push(Interface {
                name: name.clone(), state: read_trim(format!("{root}/operstate")),
                mac: read_trim(format!("{root}/address")), mtu: read_trim(format!("{root}/mtu")),
                speed_mbps: read_trim(format!("{root}/speed")).parse().ok().filter(|v: &u64| *v > 0),
                addresses: addresses.get(&name).cloned().unwrap_or_default(), counters: counters.clone(),
                rx_rate, tx_rate, rx_pps, tx_pps,
            });
        }
        self.previous = interfaces.iter().map(|i| (i.name.clone(), i.counters.clone())).collect();
        self.last = now;
        if self.last_owners.elapsed().as_secs() >= 5 {
            self.owners = process_owners();
            self.last_owners = Instant::now();
        }
        let mut connections = Vec::new();
        for (file, protocol, ipv6) in [("tcp", "TCP", false), ("tcp6", "TCP6", true), ("udp", "UDP", false), ("udp6", "UDP6", true)] {
            match fs::read_to_string(format!("/proc/net/{file}")) {
                Ok(raw) => connections.extend(parse_connections(&raw, protocol, ipv6)),
                Err(e) => warnings.push(format!("{protocol} sockets unavailable: {e}")),
            }
        }
        for c in &mut connections {
            if let Some((pid, name)) = self.owners.get(&c.inode) { c.pid = Some(*pid); c.process = Some(name.clone()); }
        }
        if connections.iter().any(|c| c.inode != 0 && c.pid.is_none()) {
            warnings.push("Some process owners are unavailable. Linux permissions restrict other users' socket details; owners refresh every 5 seconds.".into());
        }
        connections.sort_by(|a,b| a.state.cmp(&b.state).then(a.local.cmp(&b.local)));
        let connection_count = connections.len();
        let mut sockets = SocketCounts::default();
        for connection in &connections {
            if connection.protocol.starts_with("TCP") {
                sockets.tcp += 1;
            } else {
                sockets.udp += 1;
            }
            match connection.state.as_str() {
                "ESTABLISHED" => sockets.established += 1,
                "LISTEN" => sockets.listening += 1,
                _ => {}
            }
        }
        let traffic = self.tracker.sample(&connections);
        connections.truncate(2000);
        if connection_count > connections.len() { warnings.push("Connection table limited to 2,000 sockets; total count includes all sockets.".into()); }
        if !interfaces.iter().any(|i| i.name != "lo") { warnings.push("Only loopback is visible. Run Monet directly on the host to monitor its network interfaces.".into()); }
        let (rx, tx) = interfaces.iter().filter(|i| i.name != "lo").fold((0.0,0.0), |(r,t), i| (r+i.rx_rate, t+i.tx_rate));
        self.history.push_back(Point { timestamp, rx, tx });
        while self.history.len() > 300 { self.history.pop_front(); }
        let mut read_source = |path: &str| fs::read_to_string(path).unwrap_or_else(|e| {
            warnings.push(format!("{path} unavailable: {e}")); String::new()
        });
        let stats = parse_snmp(&read_source("/proc/net/snmp"));
        let routes = parse_routes(&read_source("/proc/net/route"));
        let dns = read_source("/etc/resolv.conf").lines().filter_map(|l| {
            let mut fields = l.split_whitespace();
            if fields.next() == Some("nameserver") { fields.next().map(str::to_string) } else { None }
        }).collect();
        Snapshot { timestamp, uptime: self.start.elapsed().as_secs(), interval_ms: 1000,
            hostname: read_trim("/etc/hostname"), interfaces, connections, routes, dns, stats, warnings,
            history: self.history.clone(), connection_count, sockets, traffic }
        }
    }

    #[cfg(target_os = "macos")]
    fn sample_macos(&mut self) -> Snapshot {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64().max(0.001);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let mut warnings = Vec::new();
        let counters = mac_interface_counters(&mut warnings);
        let addresses = mac_interface_addresses();
        let mut interfaces = Vec::new();
        for (name, counters) in counters {
            let old = self.previous.get(&name);
            let rate = |new: u64, old: u64| counter_rate(new, old, elapsed);
            let (rx_rate, tx_rate, rx_pps, tx_pps) = match old {
                Some(old) => (rate(counters.rx, old.rx), rate(counters.tx, old.tx), rate(counters.rx_packets, old.rx_packets), rate(counters.tx_packets, old.tx_packets)),
                None => (0.0, 0.0, 0.0, 0.0),
            };
            let details = mac_interface_details(&name);
            interfaces.push(Interface {
                name: name.clone(),
                state: details.state,
                mac: details.mac,
                mtu: details.mtu,
                speed_mbps: None,
                addresses: addresses.get(&name).cloned().unwrap_or_default(),
                counters,
                rx_rate,
                tx_rate,
                rx_pps,
                tx_pps,
            });
        }
        self.previous = interfaces.iter().map(|i| (i.name.clone(), i.counters.clone())).collect();
        self.last = now;
        let mut connections = mac_connections(&mut warnings);
        connections.sort_by(|a, b| a.state.cmp(&b.state).then(a.local.cmp(&b.local)));
        let connection_count = connections.len();
        let mut sockets = SocketCounts::default();
        for connection in &connections {
            if connection.protocol.starts_with("TCP") { sockets.tcp += 1; } else { sockets.udp += 1; }
            match connection.state.as_str() {
                "ESTABLISHED" => sockets.established += 1,
                "LISTEN" => sockets.listening += 1,
                _ => {}
            }
        }
        let traffic = FlowSnapshot { entries: Vec::new(), error: Some("Per-connection TCP counters are unavailable on macOS.".into()) };
        let (rx, tx) = interfaces.iter().filter(|i| i.name != "lo0").fold((0.0, 0.0), |(rx, tx), i| (rx + i.rx_rate, tx + i.tx_rate));
        self.history.push_back(Point { timestamp, rx, tx });
        while self.history.len() > 300 { self.history.pop_front(); }
        if interfaces.is_empty() { warnings.push("No network interfaces were reported by netstat.".into()); }
        connections.truncate(2000);
        if connection_count > connections.len() { warnings.push("Connection table limited to 2,000 sockets; total count includes all sockets.".into()); }
        Snapshot {
            timestamp,
            uptime: self.start.elapsed().as_secs(),
            interval_ms: 1000,
            hostname: command_text("hostname", &[], "unknown"),
            interfaces,
            connections,
            routes: mac_routes(&mut warnings),
            dns: fs::read_to_string("/etc/resolv.conf").unwrap_or_default().lines().filter_map(|line| {
                let mut fields = line.split_whitespace();
                if fields.next() == Some("nameserver") { fields.next().map(str::to_string) } else { None }
            }).collect(),
            stats: BTreeMap::new(),
            warnings,
            history: self.history.clone(),
            connection_count,
            sockets,
            traffic,
        }
    }
}

#[cfg(target_os = "macos")]
fn command_text(program: &str, args: &[&str], fallback: &str) -> String {
    Command::new(program).args(args).output().ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| fallback.into())
}

#[cfg(target_os = "macos")]
fn command_lines(program: &str, args: &[&str], warnings: &mut Vec<String>) -> Vec<String> {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect(),
        Ok(output) => {
            warnings.push(format!("{program} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
            Vec::new()
        }
        Err(error) => {
            warnings.push(format!("{program} unavailable: {error}"));
            Vec::new()
        }
    }
}

#[cfg(target_os = "macos")]
fn mac_interface_counters(warnings: &mut Vec<String>) -> BTreeMap<String, Counters> {
    let mut result = BTreeMap::new();
    for line in command_lines("netstat", &["-ib"], warnings) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 10 || fields[2] != "<Link#1>" && !fields[2].starts_with("<Link#") { continue; }
        let Ok(mtu) = fields[1].parse::<u64>() else { continue; };
        if mtu == 0 { continue; }
        let values: Option<Vec<u64>> = fields[4..10].iter().map(|value| value.parse().ok()).collect();
        let Some(values) = values else { continue; };
        let counters = Counters {
            rx_packets: values[0], rx_errors: values[1], rx: values[2],
            tx_packets: values[3], tx_errors: values[4], tx: values[5],
            ..Counters::default()
        };
        result.entry(fields[0].to_string()).and_modify(|old: &mut Counters| {
            old.rx = old.rx.max(counters.rx);
            old.tx = old.tx.max(counters.tx);
            old.rx_packets = old.rx_packets.max(counters.rx_packets);
            old.tx_packets = old.tx_packets.max(counters.tx_packets);
            old.rx_errors = old.rx_errors.max(counters.rx_errors);
            old.tx_errors = old.tx_errors.max(counters.tx_errors);
        }).or_insert(counters);
    }
    result
}

#[cfg(target_os = "macos")]
struct MacInterfaceDetails { state: String, mac: String, mtu: String }

#[cfg(target_os = "macos")]
fn mac_interface_details(name: &str) -> MacInterfaceDetails {
    let output = command_text("ifconfig", &[name], "");
    let state = if output.lines().any(|line| line.contains("status: active")) { "up" } else { "down" };
    let mac = output.lines().find_map(|line| line.trim().strip_prefix("ether ").and_then(|value| value.split_whitespace().next())).unwrap_or("");
    let mtu = output.lines().next().and_then(|line| line.split_whitespace().find_map(|field| field.strip_prefix("mtu "))).unwrap_or("");
    MacInterfaceDetails { state: state.into(), mac: mac.into(), mtu: mtu.into() }
}

#[cfg(target_os = "macos")]
fn mac_interface_addresses() -> HashMap<String, Vec<String>> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    let mut current = None;
    for line in command_text("ifconfig", &["-a"], "").lines() {
        if line.chars().next().is_some_and(|character| !character.is_whitespace()) {
            current = line.split(':').next().map(str::to_string);
        }
        let Some(name) = current.as_ref() else { continue; };
        let fields: Vec<_> = line.split_whitespace().collect();
        if let Some(index) = fields.iter().position(|field| *field == "inet" || *field == "inet6") {
            if let Some(address) = fields.get(index + 1) { result.entry(name.clone()).or_default().push(address.trim_end_matches('%').to_string()); }
        }
    }
    result
}

#[cfg(target_os = "macos")]
fn mac_endpoint(value: &str) -> String {
    if value == "*.*" || value == "0.0.0.0.*" { return "0.0.0.0:0".into(); }
    if let Some((host, port)) = value.rsplit_once('.') {
        if port.parse::<u16>().is_ok() {
            return if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
        }
    }
    value.into()
}

#[cfg(target_os = "macos")]
fn mac_connections(warnings: &mut Vec<String>) -> Vec<Connection> {
    let mut connections = Vec::new();
    for line in command_lines("netstat", &["-anv", "-p", "tcp"], warnings).into_iter().chain(command_lines("netstat", &["-anv", "-p", "udp"], warnings)) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 5 || !(fields[0].starts_with("tcp") || fields[0].starts_with("udp")) { continue; }
        let protocol = if fields[0].starts_with("tcp") { fields[0].to_uppercase() } else { fields[0].to_uppercase() };
        let state = fields.get(5).filter(|value| matches!(value.as_bytes().first(), Some(b'A'..=b'Z'))).copied().unwrap_or(if protocol.starts_with("TCP") { "UNKNOWN" } else { "UNCONNECTED" });
        connections.push(Connection {
            protocol,
            local: mac_endpoint(fields[3]),
            remote: mac_endpoint(fields[4]),
            state: state.into(),
            uid: 0,
            inode: 0,
            tx_queue: fields[2].parse().unwrap_or(0),
            rx_queue: fields[1].parse().unwrap_or(0),
            process: None,
            pid: None,
        });
    }
    connections
}

#[cfg(target_os = "macos")]
fn mac_routes(warnings: &mut Vec<String>) -> Vec<(String, String, String, u64)> {
    command_lines("netstat", &["-rn", "-f", "inet"], warnings).into_iter().filter_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4 || fields[0] == "Destination" || fields[0] == "Routing" { return None; }
        let destination = if fields[0] == "default" { "0.0.0.0/0".into() } else { fields[0].into() };
        Some((fields.last()?.to_string(), destination, fields[1].into(), 0))
    }).collect()
}

fn flow_json(flow: &Flow) -> String {
    let (down_rate, up_rate) = flow.rate.map_or_else(|| ("null".into(), "null".into()), |(down, up)| (num(down), num(up)));
    object(vec![
        s("local", &flow.local.to_string()), s("remote", &flow.remote.to_string()), n("inode", flow.inode),
        s("state", if flow.active { flow.state_label() } else { "CLOSED" }),
        ("process".into(), flow.process.as_deref().map_or_else(|| "null".into(), quote)),
        n("pid", flow.pid.map_or_else(|| "null".into(), |pid| pid.to_string())),
        n("download_bytes", flow.lifetime.download), n("upload_acked_bytes", flow.lifetime.upload),
        n("download_session_bytes", flow.session.download), n("upload_session_bytes", flow.session.upload),
        n("download_bytes_per_second", down_rate), n("upload_bytes_per_second", up_rate),
        n("rtt_us", flow.rtt_us), n("retransmits", flow.retransmits),
    ])
}

fn read_trim(path: impl AsRef<Path>) -> String { fs::read_to_string(path).unwrap_or_default().trim().to_string() }

pub fn parse_dev(raw: &str) -> BTreeMap<String, Counters> {
    raw.lines().filter_map(|line| {
        let (name, values) = line.split_once(':')?;
        let fields: Vec<u64> = values.split_whitespace().map(str::parse).collect::<Result<_,_>>().ok()?;
        if fields.len() < 16 { return None; }
        Some((name.trim().into(), Counters { rx: fields[0], rx_packets: fields[1], rx_errors: fields[2], rx_drops: fields[3],
            tx: fields[8], tx_packets: fields[9], tx_errors: fields[10], tx_drops: fields[11] }))
    }).collect()
}

fn address(raw: &str, ipv6: bool) -> Option<String> {
    let (ip, port) = raw.split_once(':')?;
    let port = u16::from_str_radix(port,16).ok()?;
    if ipv6 {
        if ip.len() != 32 || !ip.is_ascii() { return None; }
        let mut bytes = [0u8;16];
        for i in 0..4 { bytes[i*4..i*4+4].copy_from_slice(&u32::from_str_radix(&ip[i*8..i*8+8],16).ok()?.to_ne_bytes()); }
        Some(format!("[{}]:{port}", Ipv6Addr::from(bytes)))
    } else {
        Some(format!("{}:{port}", Ipv4Addr::from(u32::from_str_radix(ip,16).ok()?.to_ne_bytes())))
    }
}

pub fn parse_connections(raw: &str, protocol: &str, ipv6: bool) -> Vec<Connection> {
    raw.lines().skip(1).filter_map(|line| {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() < 10 { return None; }
        let (tx,rx) = f[4].split_once(':')?;
        let state = match f[3] {
            "01" => "ESTABLISHED", "02" => "SYN_SENT", "03" => "SYN_RECV", "04" => "FIN_WAIT1",
            "05" => "FIN_WAIT2", "06" => "TIME_WAIT", "07" if protocol.starts_with("UDP") => "UNCONNECTED",
            "07" => "CLOSE", "08" => "CLOSE_WAIT", "09" => "LAST_ACK", "0A" => "LISTEN", "0B" => "CLOSING", _ => "UNKNOWN",
        };
        Some(Connection { protocol: protocol.into(), local: address(f[1],ipv6)?, remote: address(f[2],ipv6)?,
            state: state.into(), uid: f[7].parse().ok()?, inode: f[9].parse().ok()?,
            tx_queue: u64::from_str_radix(tx,16).ok()?, rx_queue: u64::from_str_radix(rx,16).ok()?, process: None, pid: None })
    }).collect()
}

fn process_owners() -> HashMap<u64, (u32, String)> {
    let mut owners = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else { return owners; };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue; };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else { continue; };
        let name = read_trim(entry.path().join("comm"));
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else { continue; };
            let target = target.to_string_lossy();
            if let Some(inode) = target.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')).and_then(|s| s.parse::<u64>().ok()) {
                owners.entry(inode).or_insert_with(|| (pid,name.clone()));
            }
        }
    }
    owners
}

pub fn parse_routes(raw: &str) -> Vec<(String,String,String,u64)> {
    raw.lines().skip(1).filter_map(|line| {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() < 8 { return None; }
        let dest = u32::from_str_radix(f[1],16).ok()?;
        let gateway = u32::from_str_radix(f[2],16).ok()?;
        let mask = u32::from_str_radix(f[7],16).ok()?;
        Some((f[0].into(), format!("{}/{}",Ipv4Addr::from(dest.to_ne_bytes()),mask.count_ones()),
            Ipv4Addr::from(gateway.to_ne_bytes()).to_string(), f[6].parse().ok()?))
    }).collect()
}

pub fn parse_snmp(raw: &str) -> BTreeMap<String,u64> {
    let mut stats = BTreeMap::new();
    let mut lines = raw.lines();
    while let (Some(header),Some(values)) = (lines.next(),lines.next()) {
        let Some((group,keys)) = header.split_once(':') else { continue; };
        let Some((value_group,values)) = values.split_once(':') else { continue; };
        if group != value_group { continue; }
        for (key,value) in keys.split_whitespace().zip(values.split_whitespace()) {
            if let Ok(value) = value.parse() { stats.insert(format!("{group}.{key}"),value); }
        }
    }
    stats
}

// getifaddrs provides IPv4 + IPv6 without requiring an `ip` subprocess or a crate.
#[cfg(target_os = "linux")]
fn interface_addresses() -> HashMap<String,Vec<String>> {
    use std::{ffi::CStr, os::raw::{c_char,c_int,c_uint,c_void}, ptr};
    #[repr(C)] struct IfAddrs { next: *mut IfAddrs, name: *mut c_char, flags: c_uint, addr: *mut c_void, netmask: *mut c_void, ifu: *mut c_void, data: *mut c_void }
    extern "C" { fn getifaddrs(ptr: *mut *mut IfAddrs) -> c_int; fn freeifaddrs(ptr: *mut IfAddrs); }
    let mut result: HashMap<String,Vec<String>> = HashMap::new();
    // Linux sockaddr_in: family(2), port(2), addr(4). sockaddr_in6: family/port/flow(8), addr(16).
    unsafe {
        let mut head = ptr::null_mut();
        if getifaddrs(&mut head) != 0 { return result; }
        let mut cursor = head;
        while !cursor.is_null() {
            let item = &*cursor;
            if !item.addr.is_null() && !item.name.is_null() {
                let family = *(item.addr as *const u16);
                let raw = item.addr as *const u8;
                let addr = match family {
                    2 => { let mut bytes = [0;4]; ptr::copy_nonoverlapping(raw.add(4),bytes.as_mut_ptr(),4); Some(Ipv4Addr::from(bytes).to_string()) }
                    10 => { let mut bytes = [0;16]; ptr::copy_nonoverlapping(raw.add(8),bytes.as_mut_ptr(),16); Some(Ipv6Addr::from(bytes).to_string()) }
                    _ => None,
                };
                if let Some(addr) = addr { result.entry(CStr::from_ptr(item.name).to_string_lossy().into_owned()).or_default().push(addr); }
            }
            cursor = item.next;
        }
        freeifaddrs(head);
    }
    result
}
#[cfg(not(target_os = "linux"))]
fn interface_addresses() -> HashMap<String,Vec<String>> { HashMap::new() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn counters_and_malformed_rows() {
        let c = parse_dev("headers\n eth0: 1000 20 1 2 0 0 0 0 2000 30 3 4 0 0 0 0\nbad: x\n");
        assert_eq!(c["eth0"].tx,2000); assert_eq!(c["eth0"].rx_drops,2); assert_eq!(c.len(),1);
    }
    #[test] fn rates_use_elapsed_time_and_handle_resets() {
        assert_eq!(counter_rate(3000, 1000, 2.0), 1000.0);
        assert_eq!(counter_rate(100, 1000, 1.0), 0.0);
        assert!(counter_rate(100, 0, 0.0).is_finite());
    }
    #[test] fn ipv4_and_ipv6_endpoints() {
        assert_eq!(address("0100007F:1ED2",false).unwrap(),"127.0.0.1:7890");
        assert_eq!(address("00000000000000000000000001000000:01BB",true).unwrap(),"[::1]:443");
        assert!(address("bad",false).is_none());
    }
    #[test] fn tcp_and_udp_states_and_queues() {
        let raw = "header\n0: 0100007F:1ED2 00000000:0000 0A 0000000A:00000014 00:00000000 00000000 1000 0 1234\n";
        let c = parse_connections(raw,"TCP",false); assert_eq!(c[0].state,"LISTEN"); assert_eq!(c[0].rx_queue,20); assert_eq!(c[0].inode,1234);
        assert_eq!(parse_connections(&raw.replace(" 0A "," 07 "),"UDP",false)[0].state,"UNCONNECTED");
    }
    #[test] fn default_route_and_snmp() {
        let routes = parse_routes("header\neth0 00000000 0101A8C0 0003 0 0 100 00000000\n");
        assert_eq!(routes[0].2,"192.168.1.1"); assert_eq!(routes[0].1,"0.0.0.0/0");
        let stats = parse_snmp("Tcp: InSegs RetransSegs MaxConn\nTcp: 10 2 -1\n"); assert_eq!(stats["Tcp.RetransSegs"],2); assert!(!stats.contains_key("Tcp.MaxConn"));
    }
}
