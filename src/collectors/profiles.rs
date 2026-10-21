use super::liveness::print_roots;
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::profiles::generation_link;
use crate::storedb;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

pub struct Profiles {
    pub interval: Duration,
}

fn mtime(link: &str) -> Option<f64> {
    let m = fs::symlink_metadata(link).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// The generation the profile symlink points at, among `links`.
fn current_generation(profile: &str, links: &[(u64, String)]) -> Option<(u64, String)> {
    let target = fs::read_link(profile).ok()?;
    // A relative target resolves against the profile's directory; an
    // absolute one replaces the path entirely.
    let target = Path::new(profile).with_file_name(target);
    let (_, n) = generation_link(&target.to_string_lossy())?;
    links.iter().find(|(m, _)| *m == n).cloned()
}

/// Metrics for the profiles named by GC root links.
pub fn summarize(roots: &[(String, String)], conn: &Connection) -> Result<Vec<Family>> {
    let mut by_profile: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for (link, _) in roots {
        if let Some((profile, n)) = generation_link(link) {
            by_profile
                .entry(profile)
                .or_default()
                .push((n, link.clone()));
        }
    }
    let mut gens = Family::gauge("nix_profile_generations", "Generations of the profile.");
    let mut current = Family::gauge(
        "nix_profile_current_generation",
        "The profile's current generation.",
    );
    let mut current_ts = Family::gauge(
        "nix_profile_current_timestamp_seconds",
        "When the current generation was created.",
    );
    let mut oldest_ts = Family::gauge(
        "nix_profile_oldest_timestamp_seconds",
        "When the oldest remaining generation was created.",
    );
    let mut closure = Family::gauge(
        "nix_profile_closure_bytes",
        "NAR size of the current generation's closure.",
    );
    for (profile, links) in &by_profile {
        let l = [("profile", profile.as_str())];
        gens.push(&l, links.len() as f64);
        if let Some(t) = links
            .iter()
            .filter_map(|(_, link)| mtime(link))
            .reduce(f64::min)
        {
            oldest_ts.push(&l, t);
        }
        let Some((n, link)) = current_generation(profile, links) else {
            continue;
        };
        current.push(&l, n as f64);
        if let Some(t) = mtime(&link) {
            current_ts.push(&l, t);
        }
        if let Ok(target) = fs::read_link(&link) {
            if let Some(bytes) = storedb::closure_bytes(conn, &target.to_string_lossy())? {
                closure.push(&l, bytes);
            }
        }
    }
    Ok(vec![gens, current, current_ts, oldest_ts, closure])
}

impl Collector for Profiles {
    fn name(&self) -> &'static str {
        "profiles"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        summarize(&print_roots()?, &storedb::open(&ctx.paths.db())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::storedb::fixture::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn summarizes_generations() {
        let d = tempfile::tempdir().unwrap();
        let w = create(&d.path().join("db.sqlite"));
        let sys = add_path(&w, "/nix/store/s2-system", 100, false, false, None);
        let dep = add_path(&w, "/nix/store/dep", 23, false, false, None);
        add_ref(&w, sys, dep);
        let profiles = d.path().join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        symlink("/nix/store/s1-system", profiles.join("system-1-link")).unwrap();
        symlink("/nix/store/s2-system", profiles.join("system-2-link")).unwrap();
        symlink("system-2-link", profiles.join("system")).unwrap();
        let p = |s: &str| profiles.join(s).to_string_lossy().into_owned();
        let roots = vec![
            (p("system-1-link"), "/nix/store/s1-system".to_string()),
            (p("system-2-link"), "/nix/store/s2-system".to_string()),
            (p("system"), "/nix/store/s2-system".to_string()),
            ("/proc/1/maps".to_string(), "/nix/store/dep".to_string()),
        ];
        let conn = storedb::open(&d.path().join("db.sqlite")).unwrap();
        let text = render(&summarize(&roots, &conn).unwrap());
        let label = format!("{{profile=\"{}\"}}", p("system"));
        assert!(
            text.contains(&format!("nix_profile_generations{label} 2\n")),
            "{text}"
        );
        assert!(text.contains(&format!("nix_profile_current_generation{label} 2\n")));
        assert!(text.contains(&format!("nix_profile_closure_bytes{label} 123\n")));
        assert!(text.contains(&format!("nix_profile_current_timestamp_seconds{label} ")));
        assert!(text.contains(&format!("nix_profile_oldest_timestamp_seconds{label} ")));
    }
}
