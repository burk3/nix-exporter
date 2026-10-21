//! The collector trait, shared context, and the snapshot file writer.

use crate::metrics::{render, Family};
use crate::nixconf::NixConfig;
use crate::paths::Paths;
use crate::resolve::Resolver;
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONFIG_TTL: Duration = Duration::from_secs(300);

pub trait Collector: Send {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>>;
    /// Whether to keep serving the last good result when a cycle fails.
    fn keep_on_failure(&self) -> bool {
        false
    }
}

/// What all collectors share.
pub struct Ctx {
    pub paths: Paths,
    pub extra_machines: Vec<PathBuf>,
    pub upkeep_units: Vec<String>,
    pub resolver: Resolver,
    config: Mutex<Option<(Instant, Arc<NixConfig>)>>,
    /// Makes `nix_config` fail, standing in for a broken `nix config show`.
    #[cfg(test)]
    config_fails: bool,
}

impl Ctx {
    pub fn new(
        paths: Paths,
        extra_machines: Vec<PathBuf>,
        upkeep_units: Vec<String>,
        resolver: Resolver,
    ) -> Self {
        Ctx {
            paths,
            extra_machines,
            upkeep_units,
            resolver,
            config: Mutex::new(None),
            #[cfg(test)]
            config_fails: false,
        }
    }

    /// `nix config show`, cached for five minutes.
    pub fn nix_config(&self) -> Result<Arc<NixConfig>> {
        #[cfg(test)]
        if self.config_fails {
            anyhow::bail!("nix config show failed (test)");
        }
        let mut c = self.config.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, cfg)) = c.as_ref() {
            if at.elapsed() < CONFIG_TTL {
                return Ok(cfg.clone());
            }
        }
        let cfg = Arc::new(NixConfig::load()?);
        *c = Some((Instant::now(), cfg.clone()));
        Ok(cfg)
    }

    /// A context over fixture paths with a fixed Nix config.
    #[cfg(test)]
    pub fn for_test(paths: Paths, config: &str) -> Self {
        let ctx = Ctx::new(paths, Vec::new(), Vec::new(), Resolver::new(false));
        *ctx.config.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), Arc::new(NixConfig::parse(config))));
        ctx
    }

    /// A context over fixture paths whose `nix_config` always fails.
    #[cfg(test)]
    pub fn for_test_without_config(paths: Paths) -> Self {
        Ctx {
            config_fails: true,
            ..Ctx::new(paths, Vec::new(), Vec::new(), Resolver::new(false))
        }
    }
}

/// A Nix command that goes through the daemon: under
/// `ProtectSystem=strict` a root client cannot open the local store.
pub fn nix_command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env("NIX_REMOTE", "daemon");
    c
}

pub fn build_info() -> Family {
    Family::gauge("nix_exporter_build_info", "Version of nix-exporter.")
        .with(&[("version", env!("CARGO_PKG_VERSION"))], 1.0)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Entry {
    families: Vec<Family>,
    success: bool,
    duration: f64,
    last_success: Option<f64>,
}

/// The merged output of all collectors, rewritten atomically after every update.
pub struct Snapshot {
    path: PathBuf,
    entries: Mutex<BTreeMap<&'static str, Entry>>,
}

impl Snapshot {
    pub fn new(path: PathBuf) -> Self {
        Snapshot {
            path,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn update(
        &self,
        name: &'static str,
        result: Result<Vec<Family>>,
        duration: f64,
        keep_on_failure: bool,
    ) -> std::io::Result<()> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let prev = entries.remove(name);
        let entry = match result {
            Ok(families) => Entry {
                families,
                success: true,
                duration,
                last_success: Some(now()),
            },
            Err(e) => {
                eprintln!("collector {name} failed: {e:#}");
                let families = match (&prev, keep_on_failure) {
                    (Some(p), true) => p.families.clone(),
                    _ => Vec::new(),
                };
                Entry {
                    families,
                    success: false,
                    duration,
                    last_success: prev.and_then(|p| p.last_success),
                }
            }
        };
        entries.insert(name, entry);
        // Written under the lock so concurrent updates cannot interleave.
        self.write(&Self::render(&entries))
    }

    fn render(entries: &BTreeMap<&'static str, Entry>) -> String {
        let mut families: Vec<Family> = entries
            .values()
            .flat_map(|e| e.families.iter().cloned())
            .collect();
        let mut success = Family::gauge(
            "nix_exporter_collector_success",
            "Whether the collector's last cycle succeeded.",
        );
        let mut duration = Family::gauge(
            "nix_exporter_collector_duration_seconds",
            "How long the collector's last cycle took.",
        );
        let mut last = Family::gauge(
            "nix_exporter_collector_last_success_timestamp_seconds",
            "When the collector last succeeded.",
        );
        for (name, e) in entries {
            success.push(&[("collector", name)], if e.success { 1.0 } else { 0.0 });
            duration.push(&[("collector", name)], e.duration);
            if let Some(t) = e.last_success {
                last.push(&[("collector", name)], t);
            }
        }
        families.extend([success, duration, last, build_info()]);
        render(&families)
    }

    fn write(&self, text: &str) -> std::io::Result<()> {
        let tmp = self.path.with_extension("prom.tmp");
        fs::write(&tmp, text)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, &self.path)
    }
}

/// The message carried by a panic payload, when it is a string.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s
    } else {
        "<non-string payload>"
    }
}

/// One collection cycle: collect (a panic counts as a failure) and record the
/// result. Returns how long the collection took.
fn cycle(c: &mut dyn Collector, ctx: &Ctx, snapshot: &Snapshot) -> Duration {
    let start = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| c.collect(ctx))).unwrap_or_else(|payload| {
        Err(anyhow!(
            "collector panicked: {}",
            panic_message(payload.as_ref())
        ))
    });
    let elapsed = start.elapsed();
    if let Err(e) = snapshot.update(c.name(), result, elapsed.as_secs_f64(), c.keep_on_failure()) {
        eprintln!("writing {}: {e}", snapshot.path.display());
    }
    elapsed
}

/// Runs each collector on its own thread, forever.
pub fn run(collectors: Vec<Box<dyn Collector>>, ctx: Arc<Ctx>, snapshot: Arc<Snapshot>) -> ! {
    for mut c in collectors {
        let ctx = ctx.clone();
        let snapshot = snapshot.clone();
        thread::spawn(move || loop {
            let elapsed = cycle(c.as_mut(), &ctx, &snapshot);
            thread::sleep(c.interval().saturating_sub(elapsed));
        });
    }
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam() -> Vec<Family> {
        vec![Family::gauge("nix_test", "Test.").with(&[], 7.0)]
    }

    #[test]
    fn writes_merged_snapshot_with_status() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("a", Ok(fam()), 0.5, false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("nix_test 7\n"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"a\"} 1\n"));
        assert!(text.contains("nix_exporter_collector_duration_seconds{collector=\"a\"} 0.5\n"));
        assert!(
            text.contains("nix_exporter_collector_last_success_timestamp_seconds{collector=\"a\"}")
        );
        assert!(text.contains("nix_exporter_build_info{version=\"0.1.0\"} 1\n"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(!d.path().join("metrics.prom.tmp").exists());
    }

    #[test]
    fn failure_drops_metrics_but_keeps_last_success() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("a", Ok(fam()), 0.1, false).unwrap();
        s.update("a", Err(anyhow!("boom")), 0.1, false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("nix_test 7"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"a\"} 0\n"));
        assert!(
            text.contains("nix_exporter_collector_last_success_timestamp_seconds{collector=\"a\"}")
        );
    }

    #[test]
    fn keep_on_failure_keeps_metrics() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("slow", Ok(fam()), 0.1, true).unwrap();
        s.update("slow", Err(anyhow!("boom")), 0.1, true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("nix_test 7\n"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"slow\"} 0\n"));
    }

    struct Panicky;

    impl Collector for Panicky {
        fn name(&self) -> &'static str {
            "panicky"
        }
        fn interval(&self) -> Duration {
            Duration::from_secs(1)
        }
        fn collect(&mut self, _ctx: &Ctx) -> Result<Vec<Family>> {
            panic!("kaboom")
        }
    }

    #[test]
    fn panicking_collector_counts_as_failure() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("panicky", Ok(fam()), 0.1, false).unwrap();
        let ctx = Ctx::for_test(Paths::default(), "");
        cycle(&mut Panicky, &ctx, &s);
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("nix_test 7"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"panicky\"} 0\n"));
        // The snapshot stays usable afterwards.
        s.update("other", Ok(fam()), 0.1, false).unwrap();
    }
}
