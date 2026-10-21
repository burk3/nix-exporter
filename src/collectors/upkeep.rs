use crate::cmd;
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::upkeep_log::{parse_log, parse_show, unix_ts, RunLog};
use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

pub struct Upkeep {
    pub interval: Duration,
}

pub struct Run {
    pub unit: String,
    pub props: HashMap<String, String>,
    pub log: RunLog,
}

/// Metrics for the last run of each unit. Units that are not loaded or have
/// never run are skipped.
pub fn families(runs: &[Run]) -> Vec<Family> {
    let mut start = Family::gauge(
        "nix_upkeep_last_start_timestamp_seconds",
        "When the unit last started.",
    );
    let mut success = Family::gauge(
        "nix_upkeep_last_success",
        "Whether the unit's last run succeeded.",
    );
    let mut duration = Family::gauge(
        "nix_upkeep_last_duration_seconds",
        "How long the unit's last run took.",
    );
    let mut freed = Family::gauge(
        "nix_upkeep_last_freed_bytes",
        "Bytes freed by the unit's last run.",
    );
    let mut deleted = Family::gauge(
        "nix_upkeep_last_deleted_paths",
        "Store paths deleted by the unit's last GC.",
    );
    let mut saved = Family::gauge(
        "nix_store_hardlink_saved_bytes",
        "Space saved by hard-linking identical files, as of the last GC.",
    );
    for r in runs {
        if r.props.get("LoadState").map(String::as_str) != Some("loaded") {
            continue;
        }
        let Some(s) = unix_ts(r.props.get("ExecMainStartTimestamp")) else {
            continue;
        };
        let l = [("unit", r.unit.as_str())];
        start.push(&l, s);
        let ok = r.props.get("Result").map(String::as_str) == Some("success");
        success.push(&l, if ok { 1.0 } else { 0.0 });
        if let Some(e) = unix_ts(r.props.get("ExecMainExitTimestamp")).filter(|e| *e >= s) {
            duration.push(&l, e - s);
        }
        if let Some(b) = r.log.freed_bytes {
            freed.push(&l, b);
        }
        if let Some(n) = r.log.deleted_paths {
            deleted.push(&l, n);
        }
        if let Some(b) = r.log.hardlink_saved_bytes {
            saved.samples.clear();
            saved.push(&[], b);
        }
    }
    vec![start, success, duration, freed, deleted, saved]
}

/// The last run of each unit, from `show` (`systemctl show` output) and
/// `journal` (an invocation's journal). A unit whose `show` fails is logged
/// and skipped, and one whose `journal` fails gets an empty log; only if
/// `show` fails for every unit does this fail.
pub fn gather(
    units: &[String],
    show: impl Fn(&str) -> Result<String>,
    journal: impl Fn(&str) -> Result<String>,
) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    let mut last_err = None;
    for unit in units {
        let props = match show(unit) {
            Ok(text) => parse_show(&text),
            Err(e) => {
                eprintln!("upkeep: systemctl show {unit}: {e:#}");
                last_err = Some(e);
                continue;
            }
        };
        let log = match props.get("InvocationID").filter(|i| !i.is_empty()) {
            Some(id) => match journal(id) {
                Ok(text) => parse_log(&text),
                Err(e) => {
                    eprintln!("upkeep: journalctl for {unit}: {e:#}");
                    RunLog::default()
                }
            },
            None => RunLog::default(),
        };
        runs.push(Run {
            unit: unit.clone(),
            props,
            log,
        });
    }
    match last_err {
        Some(e) if runs.is_empty() => Err(e.context("systemctl show failed for every unit")),
        _ => Ok(runs),
    }
}

impl Collector for Upkeep {
    fn name(&self) -> &'static str {
        "upkeep"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let show = |unit: &str| {
            cmd::output(
                Command::new("systemctl").args([
                    "show",
                    unit,
                    "--timestamp=unix",
                    "-p",
                    "LoadState",
                    "-p",
                    "Result",
                    "-p",
                    "InvocationID",
                    "-p",
                    "ExecMainStartTimestamp",
                    "-p",
                    "ExecMainExitTimestamp",
                ]),
                Duration::from_secs(10),
            )
        };
        let journal = |id: &str| {
            cmd::output(
                Command::new("journalctl").args([
                    "--no-pager",
                    "-o",
                    "cat",
                    &format!("_SYSTEMD_INVOCATION_ID={id}"),
                ]),
                Duration::from_secs(30),
            )
        };
        Ok(families(&gather(&ctx.upkeep_units, show, journal)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;

    #[test]
    fn reports_last_runs() {
        let gc = Run {
            unit: "nix-gc.service".into(),
            props: parse_show("LoadState=loaded\nResult=success\nExecMainStartTimestamp=@1000\nExecMainExitTimestamp=@1045\n"),
            log: parse_log("note: hard linking is currently saving 1.0 KiB\n3 store paths deleted, 2.0 KiB freed\n"),
        };
        let missing = Run {
            unit: "nope.service".into(),
            props: parse_show("LoadState=not-found\n"),
            log: RunLog::default(),
        };
        let never = Run {
            unit: "nix-optimise.service".into(),
            props: parse_show("LoadState=loaded\nResult=success\nExecMainStartTimestamp=\n"),
            log: RunLog::default(),
        };
        let text = render(&families(&[gc, missing, never]));
        assert!(
            text.contains(
                "nix_upkeep_last_start_timestamp_seconds{unit=\"nix-gc.service\"} 1000\n"
            ),
            "{text}"
        );
        assert!(text.contains("nix_upkeep_last_success{unit=\"nix-gc.service\"} 1\n"));
        assert!(text.contains("nix_upkeep_last_duration_seconds{unit=\"nix-gc.service\"} 45\n"));
        assert!(text.contains("nix_upkeep_last_freed_bytes{unit=\"nix-gc.service\"} 2048\n"));
        assert!(text.contains("nix_upkeep_last_deleted_paths{unit=\"nix-gc.service\"} 3\n"));
        assert!(text.contains("nix_store_hardlink_saved_bytes 1024\n"));
        assert!(!text.contains("nope.service"));
        assert!(!text.contains("nix-optimise.service"));
    }

    #[test]
    fn one_failing_unit_does_not_fail_the_rest() {
        let units: Vec<String> = ["broken.service", "nix-gc.service", "nix-optimise.service"]
            .map(String::from)
            .to_vec();
        let show = |unit: &str| {
            match unit {
            "broken.service" => Err(anyhow::anyhow!("systemctl timed out")),
            "nix-gc.service" => Ok("LoadState=loaded\nResult=success\nInvocationID=gc1\nExecMainStartTimestamp=@1000\n".to_string()),
            _ => Ok("LoadState=loaded\nResult=success\nInvocationID=opt1\nExecMainStartTimestamp=@2000\n".to_string()),
        }
        };
        let journal = |id: &str| match id {
            "gc1" => Ok("3 store paths deleted, 2.0 KiB freed\n".to_string()),
            _ => Err(anyhow::anyhow!("journalctl failed")),
        };
        let runs = gather(&units, show, journal).unwrap();
        let text = render(&families(&runs));
        assert!(!text.contains("broken.service"), "{text}");
        assert!(text.contains("nix_upkeep_last_freed_bytes{unit=\"nix-gc.service\"} 2048\n"));
        assert!(text.contains(
            "nix_upkeep_last_start_timestamp_seconds{unit=\"nix-optimise.service\"} 2000\n"
        ));
        assert!(!text.contains("nix_upkeep_last_freed_bytes{unit=\"nix-optimise.service\"}"));

        let all_fail = |_: &str| -> Result<String> { Err(anyhow::anyhow!("no systemctl")) };
        assert!(gather(&units, all_fail, journal).is_err());
        assert!(gather(&[], all_fail, journal).unwrap().is_empty());
    }
}
