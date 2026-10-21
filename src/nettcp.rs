//! Remote addresses of TCP sockets by inode, from `/proc/net/tcp{,6}`.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct TcpTable {
    by_inode: HashMap<u64, IpAddr>,
}

impl TcpTable {
    pub fn read(proc_dir: &Path) -> Self {
        let mut t = TcpTable::default();
        for f in ["net/tcp", "net/tcp6"] {
            if let Ok(text) = fs::read_to_string(proc_dir.join(f)) {
                t.add(&text);
            }
        }
        t
    }

    pub fn add(&mut self, text: &str) {
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // f[2] rem_address, f[3] state (0A = LISTEN), f[9] inode
            if f.len() < 10 || f[3] == "0A" {
                continue;
            }
            let ip = f[2].split_once(':').and_then(|(a, _)| parse_addr(a));
            if let (Some(ip), Ok(inode)) = (ip, f[9].parse::<u64>()) {
                if inode != 0 {
                    self.by_inode.insert(inode, ip);
                }
            }
        }
    }

    pub fn remote(&self, inode: u64) -> Option<IpAddr> {
        self.by_inode.get(&inode).copied()
    }
}

/// The kernel prints addresses as native-endian 32-bit words in hex.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(hex, 16).ok()?.to_ne_bytes(),
        ))),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let w = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            let v6 = Ipv6Addr::from(bytes);
            Some(
                v6.to_ipv4_mapped()
                    .map(IpAddr::V4)
                    .unwrap_or(IpAddr::V6(v6)),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // From juicy-j: sshd's connection from freddie-kane (100.74.146.55), and a listener.
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0EA57364:0016 37924A64:B354 01 00000000:00000000 02:000A3B2C 00000000     0        0 11035730 2 0000000000000000 20 4 30 10 -1\n   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 37930 1 0000000000000000 100 0 0 10 0\n";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0000000000000000FFFF00000100007F:0016 0000000000000000FFFF00000100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 20 4 30 10 -1\n";

    #[test]
    fn parses_v4_and_v4_mapped_v6() {
        let mut t = TcpTable::default();
        t.add(TCP);
        t.add(TCP6);
        assert_eq!(t.remote(11035730), Some("100.74.146.55".parse().unwrap()));
        assert_eq!(t.remote(555), Some("127.0.0.1".parse().unwrap()));
        assert_eq!(t.remote(37930), None, "listeners are skipped");
    }

    #[test]
    fn non_ascii_hex_is_rejected() {
        assert_eq!(parse_addr("é0000000"), None);
    }
}
