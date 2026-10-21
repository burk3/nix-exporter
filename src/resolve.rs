//! Reverse DNS for peer addresses, cached for an hour; falls back to the address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(3600);

pub struct Resolver {
    enabled: bool,
    cache: Mutex<HashMap<IpAddr, (Instant, String)>>,
}

impl Resolver {
    pub fn new(enabled: bool) -> Self {
        Resolver {
            enabled,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn name(&self, ip: IpAddr) -> String {
        if !self.enabled {
            return ip.to_string();
        }
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, name)) = cache.get(&ip) {
            if at.elapsed() < TTL {
                return name.clone();
            }
        }
        let name = dns_lookup::lookup_addr(&ip)
            .map(|n| n.trim_end_matches('.').to_string())
            .unwrap_or_else(|_| ip.to_string());
        cache.insert(ip, (Instant::now(), name.clone()));
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_the_address() {
        let r = Resolver::new(false);
        assert_eq!(r.name("100.74.146.55".parse().unwrap()), "100.74.146.55");
    }
}
