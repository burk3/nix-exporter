use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::storedb;
use anyhow::Result;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub struct Store {
    pub interval: Duration,
}

fn size(p: &Path) -> f64 {
    fs::metadata(p).map(|m| m.len() as f64).unwrap_or(0.0)
}

impl Collector for Store {
    fn name(&self) -> &'static str {
        "store"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let db = ctx.paths.db();
        let s = storedb::stats(&storedb::open(&db)?)?;
        let db_bytes = size(&db) + size(&db.with_extension("sqlite-wal"));
        Ok(vec![
            Family::gauge("nix_store_valid_paths", "Valid store paths.").with(&[], s.paths),
            Family::gauge(
                "nix_store_nar_bytes",
                "Total NAR size of valid store paths.",
            )
            .with(&[], s.nar_bytes),
            Family::gauge(
                "nix_store_paths",
                "Valid store paths by whether this host built them.",
            )
            .with(&[("origin", "built")], s.built)
            .with(&[("origin", "substituted")], s.paths - s.built),
            Family::gauge(
                "nix_store_signed_paths",
                "Valid store paths with at least one signature.",
            )
            .with(&[], s.signed),
            Family::gauge(
                "nix_store_last_registration_timestamp_seconds",
                "When the newest store path was registered.",
            )
            .with(&[], s.last_registration),
            Family::gauge(
                "nix_store_db_bytes",
                "Size of the store database and its write-ahead log.",
            )
            .with(&[], db_bytes),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::paths::Paths;
    use crate::storedb::fixture::*;

    #[test]
    fn reports_store_stats() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            state_dir: d.path().to_path_buf(),
            ..Paths::default()
        };
        let w = create(&paths.db());
        add_path(&w, "/nix/store/a-x", 10, true, false, None);
        add_path(&w, "/nix/store/b-y", 20, false, true, None);
        let ctx = Ctx::for_test(paths, "");
        let text = render(
            &Store {
                interval: Duration::from_secs(1),
            }
            .collect(&ctx)
            .unwrap(),
        );
        assert!(text.contains("nix_store_valid_paths 2\n"), "{text}");
        assert!(text.contains("nix_store_nar_bytes 30\n"));
        assert!(text.contains("nix_store_paths{origin=\"built\"} 1\n"));
        assert!(text.contains("nix_store_paths{origin=\"substituted\"} 1\n"));
        assert!(text.contains("nix_store_signed_paths 1\n"));
        assert!(text.contains("nix_store_last_registration_timestamp_seconds 101\n"));
        assert!(text.contains("nix_store_db_bytes "));
    }
}
