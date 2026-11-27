//! Read Linux TCP_INFO for local sockets through SOCK_DIAG.
use std::{
    io,
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::raw::{c_int, c_long, c_void},
    time::{Duration, Instant},
};

const AF_NETLINK: c_int = 16;
const SOCK_DGRAM: c_int = 2;
const NETLINK_SOCK_DIAG: c_int = 4;
const SOL_SOCKET: c_int = 1;
const SO_RCVTIMEO: c_int = 20;
const MSG_TRUNC: c_int = 0x20;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSG_OVERRUN: u16 = 4;
const NLM_F_DUMP_INTR: u16 = 0x10;
const HEADER_LEN: usize = 16;
const DIAG_LEN: usize = 72;
const INET_DIAG_INFO: u16 = 2;

#[repr(C)]
struct NetlinkAddr {
    family: u16,
    pad: u16,
    pid: u32,
    groups: u32,
}

#[repr(C)]
struct Timeval {
    seconds: c_long,
    microseconds: c_long,
}

extern "C" {
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn setsockopt(fd: c_int, level: c_int, name: c_int, value: *const c_void, size: u32) -> c_int;
    fn sendto(fd: c_int, buf: *const u8, len: usize, flags: c_int, addr: *const NetlinkAddr, addrlen: u32) -> isize;
    fn recvfrom(fd: c_int, buf: *mut u8, len: usize, flags: c_int, addr: *mut NetlinkAddr, addrlen: *mut u32) -> isize;
}

#[derive(Debug, Clone, Copy)]
pub struct TcpSample {
    pub cookie: u64,
    pub inode: u64,
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub state: u8,
    pub download: u64,
    pub upload_acked: u64,
    pub rtt_us: u32,
    pub retransmits: u32,
}

pub fn collect() -> io::Result<Vec<TcpSample>> {
    let mut samples = Vec::new();
    dump(AF_INET, &mut samples)?;
    match dump(AF_INET6, &mut samples) {
        // Some IPv4-only kernels do not offer a diag handler for IPv6.
        Err(error) if error.raw_os_error() == Some(97) => {}
        result => result?,
    }
    Ok(samples)
}

fn dump(family: u8, samples: &mut Vec<TcpSample>) -> io::Result<()> {
    // SAFETY: the arguments are Linux socket constants. A successful fd is owned once.
    let fd = unsafe { socket(AF_NETLINK, SOCK_DGRAM, NETLINK_SOCK_DIAG) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let timeout = Timeval { seconds: 0, microseconds: 200_000 };
    // SAFETY: the option value is a Linux timeval with the advertised length.
    let rc = unsafe {
        setsockopt(fd.as_raw_fd(), SOL_SOCKET, SO_RCVTIMEO, (&timeout as *const Timeval).cast(), size_of::<Timeval>() as u32)
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    let request = request(family);
    let kernel = NetlinkAddr { family: AF_NETLINK as u16, pad: 0, pid: 0, groups: 0 };
    // SAFETY: request and address remain live for the duration of the send.
    let sent = unsafe {
        sendto(fd.as_raw_fd(), request.as_ptr(), request.len(), 0, &kernel, size_of::<NetlinkAddr>() as u32)
    };
    if sent < 0 {
        return Err(io::Error::last_os_error());
    }
    if sent as usize != request.len() {
        return Err(invalid("short SOCK_DIAG request"));
    }

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut buffer = vec![0; 65_536];
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "SOCK_DIAG dump timed out"));
        }
        let mut sender = NetlinkAddr { family: 0, pad: 0, pid: 0, groups: 0 };
        let mut sender_len = size_of::<NetlinkAddr>() as u32;
        // SAFETY: all output pointers refer to writable buffers of the supplied sizes.
        let len = unsafe {
            recvfrom(fd.as_raw_fd(), buffer.as_mut_ptr(), buffer.len(), MSG_TRUNC, &mut sender, &mut sender_len)
        };
        if len < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if sender_len as usize != size_of::<NetlinkAddr>() || sender.family != AF_NETLINK as u16 || sender.pid != 0 {
            continue;
        }
        if len as usize > buffer.len() || len == 0 {
            return Err(invalid("truncated SOCK_DIAG datagram"));
        }
        if parse_datagram(&buffer[..len as usize], samples)? {
            return Ok(());
        }
    }
}

fn request(family: u8) -> [u8; 72] {
    // nlmsghdr (16 bytes) followed by inet_diag_req_v2 (56 bytes).
    let mut packet = [0; 72];
    packet[..4].copy_from_slice(&72_u32.to_ne_bytes());
    packet[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    packet[6..8].copy_from_slice(&0x301_u16.to_ne_bytes()); // REQUEST | ROOT | MATCH
    packet[8..12].copy_from_slice(&1_u32.to_ne_bytes());
    packet[16] = family;
    packet[17] = 6; // IPPROTO_TCP
    packet[18] = 1 << (INET_DIAG_INFO - 1);
    // Exclude listeners and time-wait records, which have no transfer counters.
    let states = !(1_u32 << 10 | 1 << 6);
    packet[20..24].copy_from_slice(&states.to_ne_bytes());
    packet[64..72].fill(0xff); // INET_DIAG_NOCOOKIE (request for all matching sockets)
    packet
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn parse_datagram(mut data: &[u8], samples: &mut Vec<TcpSample>) -> io::Result<bool> {
    while data.len() >= HEADER_LEN {
        let length = u32_at(data, 0).unwrap() as usize;
        let kind = u16_at(data, 4).unwrap();
        let flags = u16_at(data, 6).unwrap();
        let seq = u32_at(data, 8).unwrap();
        if !(HEADER_LEN..=data.len()).contains(&length) {
            return Err(invalid("invalid SOCK_DIAG message length"));
        }
        let payload = &data[HEADER_LEN..length];
        if seq == 1 {
            if flags & NLM_F_DUMP_INTR != 0 || kind == NLMSG_OVERRUN {
                return Err(invalid("interrupted SOCK_DIAG dump"));
            }
            if kind == NLMSG_DONE || kind == NLMSG_ERROR {
                if let Some(error) = u32_at(payload, 0).map(|v| v as i32) {
                    if error < 0 {
                        return Err(io::Error::from_raw_os_error(error.saturating_neg()));
                    }
                } else if kind == NLMSG_ERROR {
                    return Err(invalid("short SOCK_DIAG error message"));
                }
                if kind == NLMSG_DONE {
                    return Ok(true);
                }
            } else if kind == SOCK_DIAG_BY_FAMILY {
                if let Some(sample) = parse_sample(payload) {
                    samples.push(sample);
                }
            }
        }
        if length == data.len() {
            return Ok(false);
        }
        data = data.get(align(length)..).ok_or_else(|| invalid("truncated SOCK_DIAG padding"))?;
    }
    if data.is_empty() { Ok(false) } else { Err(invalid("short SOCK_DIAG header")) }
}

fn parse_sample(data: &[u8]) -> Option<TcpSample> {
    let family = *data.first()?;
    let local = endpoint(family, data.get(8..24)?, &data[4..6])?;
    let remote = endpoint(family, data.get(24..40)?, &data[6..8])?;
    let inode = u64::from(u32_at(data, 68)?);
    if inode == 0 || remote.port() == 0 || remote.ip().is_unspecified() {
        return None;
    }
    let mut attrs = data.get(DIAG_LEN..)?;
    while attrs.len() >= 4 {
        let length = usize::from(u16_at(attrs, 0)?);
        let kind = u16_at(attrs, 2)? & 0x3fff;
        if !(4..=attrs.len()).contains(&length) {
            return None;
        }
        if kind == INET_DIAG_INFO {
            let info = &attrs[4..length];
            // Offsets in Linux's append-only struct tcp_info. Old kernels can
            // return a shorter buffer, in which case counters are unavailable.
            return Some(TcpSample {
                cookie: u64::from(u32_at(data, 44)?) | (u64::from(u32_at(data, 48)?) << 32),
                inode,
                local,
                remote,
                state: data[1],
                download: u64_at(info, 128)?,
                upload_acked: u64_at(info, 120)?,
                rtt_us: u32_at(info, 68)?,
                retransmits: u32_at(info, 100)?,
            });
        }
        attrs = attrs.get(align(length)..)?;
    }
    None
}

fn endpoint(family: u8, addr: &[u8], port: &[u8]) -> Option<SocketAddr> {
    let port = u16::from_be_bytes(port.try_into().ok()?);
    match family {
        AF_INET => Some((Ipv4Addr::from(<[u8; 4]>::try_from(&addr[..4]).ok()?), port).into()),
        AF_INET6 => Some((Ipv6Addr::from(<[u8; 16]>::try_from(addr).ok()?), port).into()),
        _ => None,
    }
}

fn align(len: usize) -> usize { (len + 3) & !3 }

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(data.get(offset..offset + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(data.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_tcp_info_carries_byte_totals_and_peer() {
        let mut msg = [0; HEADER_LEN + DIAG_LEN + 4 + 136];
        let length = msg.len() as u32;
        msg[..4].copy_from_slice(&length.to_ne_bytes());
        msg[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
        msg[8..12].copy_from_slice(&1_u32.to_ne_bytes());
        let diag = &mut msg[HEADER_LEN..];
        diag[0] = AF_INET6;
        diag[1] = 1;
        diag[4..6].copy_from_slice(&41391_u16.to_be_bytes());
        diag[6..8].copy_from_slice(&443_u16.to_be_bytes());
        diag[8..24].copy_from_slice(&Ipv6Addr::LOCALHOST.octets());
        diag[24..40].copy_from_slice(&"2001:db8::42".parse::<Ipv6Addr>().unwrap().octets());
        diag[44..48].copy_from_slice(&11_u32.to_ne_bytes());
        diag[68..72].copy_from_slice(&24_u32.to_ne_bytes());
        diag[72..74].copy_from_slice(&140_u16.to_ne_bytes());
        diag[74..76].copy_from_slice(&INET_DIAG_INFO.to_ne_bytes());
        let info = &mut diag[76..];
        info[68..72].copy_from_slice(&2500_u32.to_ne_bytes());
        info[100..104].copy_from_slice(&3_u32.to_ne_bytes());
        info[120..128].copy_from_slice(&818_u64.to_ne_bytes());
        info[128..136].copy_from_slice(&9001_u64.to_ne_bytes());

        let mut samples = Vec::new();
        assert!(!parse_datagram(&msg, &mut samples).unwrap());
        let sample = samples[0];
        assert_eq!(sample.remote.to_string(), "[2001:db8::42]:443");
        assert_eq!((sample.cookie, sample.inode, sample.download, sample.upload_acked), (11, 24, 9001, 818));
        assert_eq!((sample.rtt_us, sample.retransmits), (2500, 3));
        assert!(parse_datagram(&msg[..msg.len() - 1], &mut samples).is_err());
    }

    #[test]
    fn interrupted_dumps_and_kernel_errors_are_not_empty_samples() {
        let mut done = [0; 20];
        done[..4].copy_from_slice(&20_u32.to_ne_bytes());
        done[4..6].copy_from_slice(&NLMSG_DONE.to_ne_bytes());
        done[8..12].copy_from_slice(&1_u32.to_ne_bytes());
        assert!(parse_datagram(&done, &mut Vec::new()).unwrap());
        done[6..8].copy_from_slice(&NLM_F_DUMP_INTR.to_ne_bytes());
        assert!(parse_datagram(&done, &mut Vec::new()).is_err());
        done[6..8].fill(0);
        done[16..].copy_from_slice(&(-1_i32).to_ne_bytes());
        assert_eq!(parse_datagram(&done, &mut Vec::new()).unwrap_err().raw_os_error(), Some(1));
    }
}
