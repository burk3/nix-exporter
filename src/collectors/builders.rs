use crate::cmd;
use crate::collector::{nix_command, Collector, Ctx};
use crate::machines;
use crate::metrics::Family;
use anyhow::Result;
use std::time::{Duration, Instant};

pub struct Builders {
    pub interval: Duration,
}

impl Collector for Builders {
    fn name(&self) -> &'static str {
        "builders"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let cfg = ctx.nix_config()?;
        let mut up = Family::gauge(
            "nix_builder_up",
            "Whether the last ping of the remote builder succeeded.",
        );
        let mut secs = Family::gauge(
            "nix_builder_ping_seconds",
            "Duration of the last ping of the remote builder.",
        );
        for m in machines::load(cfg.get("builders"), &ctx.extra_machines)
            .iter()
            .filter(|m| !m.is_local())
        {
            let start = Instant::now();
            let ok = cmd::status(
                nix_command("nix").args([
                    "--extra-experimental-features",
                    "nix-command",
                    "store",
                    "info",
                    "--store",
                    &m.ping_uri(),
                ]),
                Duration::from_secs(10),
            )
            .is_ok();
            up.push(&[("machine", &m.uri)], if ok { 1.0 } else { 0.0 });
            secs.push(&[("machine", &m.uri)], start.elapsed().as_secs_f64());
        }
        Ok(vec![up, secs])
    }
}
