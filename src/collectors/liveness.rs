use crate::cmd;
use crate::collector::{nix_command, Collector, Ctx};
use crate::liveness::{self, Graph};
use crate::metrics::Family;
use crate::storedb;
use anyhow::Result;
use std::collections::BTreeMap;
use std::time::Duration;

pub struct Liveness {
    pub interval: Duration,
}

/// `(link, store path)` for every GC root, via the daemon.
pub fn print_roots() -> Result<Vec<(String, String)>> {
    let text = cmd::output(
        nix_command("nix-store").args(["--gc", "--print-roots"]),
        Duration::from_secs(120),
    )?;
    Ok(liveness::parse_roots(&text))
}

struct Computed {
    graph: Graph,
    dead: Vec<u32>,
    roots: Vec<(String, String)>,
}

fn compute(ctx: &Ctx) -> Result<Computed> {
    // Temp roots are read before loading the graph so paths built
    // meanwhile stay alive.
    let roots = print_roots()?;
    let store = ctx.paths.store_dir.as_str();
    let mut root_paths: Vec<String> = roots
        .iter()
        .filter_map(|(_, t)| liveness::store_path_of(t, store))
        .collect();
    root_paths.extend(
        liveness::temp_roots(&ctx.paths.temproots())
            .iter()
            .filter_map(|t| liveness::store_path_of(t, store)),
    );
    let cfg = ctx.nix_config()?;
    let graph = Graph::load(&storedb::open(&ctx.paths.db())?)?;
    let dead = graph.dead(
        &root_paths,
        cfg.flag("keep-derivations"),
        cfg.flag("keep-outputs"),
    );
    Ok(Computed { graph, dead, roots })
}

/// The dead store paths, sorted; for `nix-exporter print-dead`.
pub fn dead_paths(ctx: &Ctx) -> Result<Vec<String>> {
    let c = compute(ctx)?;
    let mut v: Vec<String> = c
        .dead
        .iter()
        .map(|i| c.graph.paths[*i as usize].clone())
        .collect();
    v.sort();
    Ok(v)
}

impl Collector for Liveness {
    fn name(&self) -> &'static str {
        "liveness"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn keep_on_failure(&self) -> bool {
        true
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let c = compute(ctx)?;
        let dead_bytes: u64 = c.dead.iter().map(|i| c.graph.nar[*i as usize]).sum();
        let state_dir = ctx.paths.state_dir.to_string_lossy();
        let mut kinds: BTreeMap<&str, f64> = BTreeMap::new();
        for (link, _) in &c.roots {
            *kinds
                .entry(liveness::root_kind(link, &state_dir))
                .or_default() += 1.0;
        }
        let mut roots = Family::gauge("nix_store_gc_roots", "GC roots by kind.");
        for (kind, n) in &kinds {
            roots.push(&[("kind", kind)], *n);
        }
        Ok(vec![
            Family::gauge(
                "nix_store_dead_paths",
                "Valid store paths not reachable from any GC root.",
            )
            .with(&[], c.dead.len() as f64),
            Family::gauge(
                "nix_store_dead_bytes",
                "NAR size of store paths a GC would delete.",
            )
            .with(&[], dead_bytes as f64),
            roots,
        ])
    }
}
