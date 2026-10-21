mod active_builds;
mod attribution;
mod buildset;
mod cgroup;
mod cmd;
mod collector;
mod collectors;
mod liveness;
mod locks;
mod machines;
mod metrics;
mod nettcp;
mod nixconf;
mod paths;
mod procs;
mod profiles;
mod resolve;
mod serve;
mod storedb;
mod upkeep_log;
mod users;

#[cfg(test)]
mod testutil;

use clap::{Parser, Subcommand};
use collector::{Ctx, Snapshot};
use paths::Paths;
use resolve::Resolver;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_SNAPSHOT: &str = "/run/nix-exporter/metrics.prom";

#[derive(Parser)]
#[command(version, about = "Prometheus exporter for the Nix store and daemon")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Collect metrics (as root) and keep writing them to a snapshot file.
    Collect {
        #[arg(long, default_value = DEFAULT_SNAPSHOT)]
        output: PathBuf,
        /// Seconds between fast collector cycles; profiles and upkeep use 4x.
        #[arg(long, default_value_t = 15)]
        base_interval: u64,
        /// Seconds between dead-path computations.
        #[arg(long, default_value_t = 3600)]
        liveness_interval: u64,
        /// Seconds between ssh pings of each remote builder.
        #[arg(long, default_value_t = 300)]
        ping_interval: u64,
        /// Do not ping remote builders (leaves out the builders collector).
        #[arg(long)]
        no_ping_builders: bool,
        /// Extra machines file to read builders from (repeatable).
        #[arg(long = "machines-file")]
        machines_files: Vec<PathBuf>,
        /// systemd unit whose last run to report (repeatable).
        #[arg(long = "upkeep-unit", default_values = ["nix-gc.service", "nix-optimise.service"])]
        upkeep_units: Vec<String>,
        /// Report remote clients by address instead of reverse DNS name.
        #[arg(long)]
        no_resolve_peers: bool,
    },
    /// Serve the snapshot file over HTTP (unprivileged).
    Serve {
        #[arg(long, default_value = "127.0.0.1:9944")]
        listen: String,
        #[arg(long, default_value = DEFAULT_SNAPSHOT)]
        input: PathBuf,
    },
    /// Print the store paths a garbage collection would delete, sorted.
    PrintDead,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Collect {
            output,
            base_interval,
            liveness_interval,
            ping_interval,
            no_ping_builders,
            machines_files,
            upkeep_units,
            no_resolve_peers,
        } => {
            let ctx = Ctx::new(
                Paths::default(),
                machines_files,
                upkeep_units,
                Resolver::new(!no_resolve_peers),
            );
            collector::run(
                collectors::all(
                    Duration::from_secs(base_interval),
                    Duration::from_secs(liveness_interval),
                    (!no_ping_builders).then(|| Duration::from_secs(ping_interval)),
                ),
                Arc::new(ctx),
                Arc::new(Snapshot::new(output)),
            )
        }
        Cmd::Serve { listen, input } => serve::serve(&listen, &input),
        Cmd::PrintDead => {
            let ctx = Ctx::new(
                Paths::default(),
                Vec::new(),
                Vec::new(),
                Resolver::new(false),
            );
            for p in collectors::dead_paths(&ctx)? {
                println!("{p}");
            }
            Ok(())
        }
    }
}
