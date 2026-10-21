//! One module per collector.

mod builders;
mod builds;
mod daemon;
mod liveness;
mod profiles;
mod slots;
mod store;
mod upkeep;

pub use liveness::dead_paths;

use crate::collector::Collector;
use std::time::Duration;

/// Every collector. `base` is the fast interval; profiles and upkeep use 4x.
/// `ping` is the builders collector's interval, or `None` to leave it out.
pub fn all(base: Duration, liveness: Duration, ping: Option<Duration>) -> Vec<Box<dyn Collector>> {
    let slow = base * 4;
    let mut v: Vec<Box<dyn Collector>> = vec![
        Box::new(store::Store { interval: base }),
        Box::new(daemon::Daemon::new(base)),
        Box::new(builds::Builds { interval: base }),
        Box::new(slots::Slots { interval: base }),
        Box::new(profiles::Profiles { interval: slow }),
        Box::new(upkeep::Upkeep { interval: slow }),
        Box::new(liveness::Liveness { interval: liveness }),
    ];
    if let Some(ping) = ping {
        v.push(Box::new(builders::Builders { interval: ping }));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builders_interval(ping: Option<Duration>) -> Option<Duration> {
        all(Duration::from_secs(15), Duration::from_secs(3600), ping)
            .iter()
            .find(|c| c.name() == "builders")
            .map(|c| c.interval())
    }

    #[test]
    fn builders_use_the_ping_interval_or_are_left_out() {
        assert_eq!(
            builders_interval(Some(Duration::from_secs(300))),
            Some(Duration::from_secs(300))
        );
        assert_eq!(builders_interval(None), None);
    }
}
