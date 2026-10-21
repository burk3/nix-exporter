//! Prometheus text exposition format (version 0.0.4).

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gauge,
    Counter,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub name: String,
    pub help: String,
    pub kind: Kind,
    pub samples: Vec<Sample>,
}

impl Family {
    pub fn gauge(name: &str, help: &str) -> Self {
        Self::new(name, help, Kind::Gauge)
    }

    pub fn counter(name: &str, help: &str) -> Self {
        Self::new(name, help, Kind::Counter)
    }

    fn new(name: &str, help: &str, kind: Kind) -> Self {
        Family {
            name: name.to_string(),
            help: help.to_string(),
            kind,
            samples: Vec::new(),
        }
    }

    pub fn push(&mut self, labels: &[(&str, &str)], value: f64) {
        self.samples.push(Sample {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            value,
        });
    }

    pub fn with(mut self, labels: &[(&str, &str)], value: f64) -> Self {
        self.push(labels, value);
        self
    }
}

fn escape_label(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn escape_help(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n")
}

fn format_value(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf" } else { "-Inf" }.to_string()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Renders families in order. Families without samples are skipped.
pub fn render(families: &[Family]) -> String {
    let mut out = String::new();
    for f in families.iter().filter(|f| !f.samples.is_empty()) {
        let kind = match f.kind {
            Kind::Gauge => "gauge",
            Kind::Counter => "counter",
        };
        let _ = writeln!(out, "# HELP {} {}", f.name, escape_help(&f.help));
        let _ = writeln!(out, "# TYPE {} {}", f.name, kind);
        for s in &f.samples {
            out.push_str(&f.name);
            if !s.labels.is_empty() {
                out.push('{');
                for (i, (k, v)) in s.labels.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "{k}=\"{}\"", escape_label(v));
                }
                out.push('}');
            }
            let _ = writeln!(out, " {}", format_value(s.value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gauge_with_labels() {
        let f = Family::gauge("nix_x", "An x.").with(&[("a", "1"), ("b", "two")], 3.0);
        assert_eq!(
            render(&[f]),
            "# HELP nix_x An x.\n# TYPE nix_x gauge\nnix_x{a=\"1\",b=\"two\"} 3\n"
        );
    }

    #[test]
    fn renders_counter_without_labels_and_fraction() {
        let f = Family::counter("nix_y_total", "A y.").with(&[], 1.5);
        assert_eq!(
            render(&[f]),
            "# HELP nix_y_total A y.\n# TYPE nix_y_total counter\nnix_y_total 1.5\n"
        );
    }

    #[test]
    fn escapes_label_values_and_help() {
        let f = Family::gauge("nix_z", "line\\one\ntwo").with(&[("l", "a\"b\\c\nd")], 0.0);
        assert_eq!(
            render(&[f]),
            "# HELP nix_z line\\\\one\\ntwo\n# TYPE nix_z gauge\nnix_z{l=\"a\\\"b\\\\c\\nd\"} 0\n"
        );
    }

    #[test]
    fn skips_empty_families() {
        assert_eq!(render(&[Family::gauge("nix_empty", "Nothing.")]), "");
    }
}
