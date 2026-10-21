//! The unprivileged HTTP side: serves the snapshot file.

use crate::collector::build_info;
use crate::metrics::{render, Family};
use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// The snapshot plus its age, or just build info if there is no snapshot yet.
pub fn body(input: &Path, now: SystemTime) -> String {
    let text = fs::read_to_string(input);
    let modified = fs::metadata(input).and_then(|m| m.modified());
    match (text, modified) {
        (Ok(text), Ok(m)) => {
            let age = now
                .duration_since(m)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            text + &render(&[Family::gauge(
                "nix_exporter_snapshot_age_seconds",
                "Seconds since the collector last wrote its snapshot.",
            )
            .with(&[], age)])
        }
        _ => render(&[build_info()]),
    }
}

pub fn serve(listen: &str, input: &Path) -> Result<()> {
    let server =
        tiny_http::Server::http(listen).map_err(|e| anyhow!("listening on {listen}: {e}"))?;
    let content_type =
        tiny_http::Header::from_bytes("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .expect("valid header");
    for request in server.incoming_requests() {
        let path = request.url().split('?').next().unwrap_or("");
        let response = if path == "/metrics" {
            tiny_http::Response::from_string(body(input, SystemTime::now()))
                .with_header(content_type.clone())
        } else {
            tiny_http::Response::from_string("nix-exporter: metrics are at /metrics\n")
                .with_status_code(404)
        };
        let _ = request.respond(response);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn appends_age_to_snapshot() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("metrics.prom");
        fs::write(&f, "nix_store_valid_paths 3\n").unwrap();
        let later = fs::metadata(&f).unwrap().modified().unwrap() + Duration::from_secs(5);
        let text = body(&f, later);
        assert!(text.starts_with("nix_store_valid_paths 3\n"));
        assert!(
            text.contains("nix_exporter_snapshot_age_seconds 5\n"),
            "{text}"
        );
    }

    #[test]
    fn missing_snapshot_serves_build_info() {
        let text = body(Path::new("/nonexistent/metrics.prom"), SystemTime::now());
        assert!(text.contains("nix_exporter_build_info{version=\"0.1.0\"} 1\n"));
        assert!(!text.contains("snapshot_age"));
    }
}
