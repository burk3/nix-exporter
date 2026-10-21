//! What a garbage collection would delete.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The store's reference graph, loaded into memory.
pub struct Graph {
    pub paths: Vec<String>,
    pub nar: Vec<u64>,
    refs: Vec<Vec<u32>>,
    deriver: Vec<Option<u32>>,
    outputs: Vec<Vec<u32>>,
    index: HashMap<String, u32>,
}

impl Graph {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut paths = Vec::new();
        let mut nar = Vec::new();
        let mut derivers: Vec<Option<String>> = Vec::new();
        let mut by_id: HashMap<i64, u32> = HashMap::new();
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut stmt =
            conn.prepare("select id, path, coalesce(narSize, 0), deriver from ValidPaths")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let i = paths.len() as u32;
            let path: String = r.get(1)?;
            by_id.insert(r.get(0)?, i);
            index.insert(path.clone(), i);
            paths.push(path);
            nar.push(r.get::<_, i64>(2)? as u64);
            derivers.push(r.get(3)?);
        }
        let n = paths.len();
        let deriver = derivers
            .iter()
            .map(|d| d.as_ref().and_then(|d| index.get(d).copied()))
            .collect();
        let mut refs = vec![Vec::new(); n];
        let mut stmt = conn.prepare("select referrer, reference from Refs")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let (Some(&a), Some(&b)) = (by_id.get(&r.get(0)?), by_id.get(&r.get(1)?)) {
                refs[a as usize].push(b);
            }
        }
        let mut outputs = vec![Vec::new(); n];
        let mut stmt = conn.prepare("select drv, path from DerivationOutputs")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let path: String = r.get(1)?;
            if let (Some(&d), Some(&o)) = (by_id.get(&r.get(0)?), index.get(&path)) {
                outputs[d as usize].push(o);
            }
        }
        Ok(Graph {
            paths,
            nar,
            refs,
            deriver,
            outputs,
            index,
        })
    }

    /// Indices of paths not reachable from `roots`. With `keep_derivations`
    /// the derivers of live paths are live; with `keep_outputs` the outputs
    /// of live derivations are live (Nix's GC settings).
    pub fn dead(&self, roots: &[String], keep_derivations: bool, keep_outputs: bool) -> Vec<u32> {
        let mut alive = vec![false; self.paths.len()];
        let mut stack: Vec<u32> = roots
            .iter()
            .filter_map(|r| self.index.get(r).copied())
            .collect();
        while let Some(i) = stack.pop() {
            let iu = i as usize;
            if alive[iu] {
                continue;
            }
            alive[iu] = true;
            stack.extend(&self.refs[iu]);
            if keep_derivations {
                stack.extend(self.deriver[iu]);
            }
            if keep_outputs {
                stack.extend(&self.outputs[iu]);
            }
        }
        (0..self.paths.len() as u32)
            .filter(|i| !alive[*i as usize])
            .collect()
    }
}

/// `nix-store --gc --print-roots` lines: `<link> -> <store path>`. Nix
/// prints the link as a quoted C++ path (`"/a/b" -> /nix/store/…`), with `"`
/// and `\` escaped; `{censored}` and older Nix leave it bare.
pub fn parse_roots(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.rsplit_once(" -> "))
        .map(|(link, target)| (unquote(link), target.to_string()))
        .collect()
}

fn unquote(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The store path containing `p` (`<store_dir>/<name>`), if `p` is in the store.
pub fn store_path_of(p: &str, store_dir: &str) -> Option<String> {
    let rest = p.strip_prefix(store_dir)?.strip_prefix('/')?;
    let name = rest.split('/').next().filter(|n| !n.is_empty())?;
    Some(format!("{store_dir}/{name}"))
}

/// Store paths in `temproots/`: one file per process, NUL-terminated paths.
pub fn temp_roots(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| fs::read(e.path()).ok())
        .flat_map(|b| {
            b.split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `kind` label for a root's link.
pub fn root_kind(link: &str, state_dir: &str) -> &'static str {
    if link.starts_with("/proc/") || link.starts_with("{censored}") {
        "runtime"
    } else if crate::profiles::generation_link(link).is_some() {
        "profile"
    } else if link.contains("/gcroots/hydra/") {
        "hydra"
    } else if link.starts_with(state_dir) || link.starts_with("/run/") {
        "other"
    } else {
        "auto"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storedb::{self, fixture::*};
    use crate::testutil::write;

    /// app -> lib -> libc; drv built app; old is unreferenced; tool.drv's
    /// output is tool.
    fn graph() -> (tempfile::TempDir, Graph) {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("db.sqlite");
        let w = create(&db);
        let drv = add_path(&w, "/nix/store/d-app.drv", 1, false, false, None);
        let app = add_path(
            &w,
            "/nix/store/a-app",
            100,
            false,
            false,
            Some("/nix/store/d-app.drv"),
        );
        let lib = add_path(&w, "/nix/store/b-lib", 50, false, false, None);
        let libc = add_path(&w, "/nix/store/c-libc", 25, false, false, None);
        add_path(&w, "/nix/store/e-old", 7, false, false, None);
        let tool_drv = add_path(&w, "/nix/store/f-tool.drv", 1, false, false, None);
        add_path(
            &w,
            "/nix/store/g-tool",
            3,
            false,
            false,
            Some("/nix/store/f-tool.drv"),
        );
        add_ref(&w, app, lib);
        add_ref(&w, lib, libc);
        add_ref(&w, lib, lib);
        add_output(&w, tool_drv, "out", "/nix/store/g-tool");
        add_output(&w, drv, "out", "/nix/store/a-app");
        let g = Graph::load(&storedb::open(&db).unwrap()).unwrap();
        (d, g)
    }

    fn names(g: &Graph, dead: &[u32]) -> Vec<String> {
        let mut v: Vec<String> = dead.iter().map(|i| g.paths[*i as usize].clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn walks_references() {
        let (_d, g) = graph();
        let dead = g.dead(
            &["/nix/store/a-app".into(), "/nix/store/missing".into()],
            false,
            false,
        );
        assert_eq!(
            names(&g, &dead),
            vec![
                "/nix/store/d-app.drv",
                "/nix/store/e-old",
                "/nix/store/f-tool.drv",
                "/nix/store/g-tool"
            ]
        );
    }

    #[test]
    fn keep_derivations_and_outputs() {
        let (_d, g) = graph();
        let dead = g.dead(&["/nix/store/a-app".into()], true, false);
        assert_eq!(
            names(&g, &dead),
            vec![
                "/nix/store/e-old",
                "/nix/store/f-tool.drv",
                "/nix/store/g-tool"
            ]
        );
        let dead = g.dead(&["/nix/store/f-tool.drv".into()], false, true);
        assert!(!names(&g, &dead).contains(&"/nix/store/g-tool".to_string()));
    }

    #[test]
    fn parses_roots_and_store_paths() {
        // Real output: links are quoted (Determinate 2.35.3 and upstream 2.34.8).
        let roots = parse_roots("\"/proc/2453/maps\" -> /nix/store/x-glibc-2.42\n\"/home/burke/.local/state/nix/profiles/home-manager-8-link\" -> /nix/store/y-home-manager-generation\n{censored} -> /nix/store/z-thing\n\"/tmp/we\\\"ird\\\\dir/result\" -> /nix/store/w-x\n/old/unquoted -> /nix/store/v-y\n");
        assert_eq!(roots.len(), 5);
        assert_eq!(
            roots[0],
            ("/proc/2453/maps".into(), "/nix/store/x-glibc-2.42".into())
        );
        assert_eq!(
            roots[1].0,
            "/home/burke/.local/state/nix/profiles/home-manager-8-link"
        );
        assert_eq!(roots[2].0, "{censored}");
        assert_eq!(roots[3].0, "/tmp/we\"ird\\dir/result");
        assert_eq!(roots[4].0, "/old/unquoted");
        assert_eq!(
            store_path_of("/nix/store/x-glibc-2.42/lib/libc.so.6", "/nix/store").as_deref(),
            Some("/nix/store/x-glibc-2.42")
        );
        assert_eq!(store_path_of("/home/x", "/nix/store"), None);
        assert_eq!(store_path_of("/nix/store/", "/nix/store"), None);
    }

    #[test]
    fn classifies_roots() {
        let s = "/nix/var/nix";
        assert_eq!(root_kind("/proc/2453/maps", s), "runtime");
        assert_eq!(root_kind("{censored}", s), "runtime");
        assert_eq!(
            root_kind("/nix/var/nix/profiles/system-55-link", s),
            "profile"
        );
        assert_eq!(
            root_kind("/home/b/.local/state/nix/profiles/home-manager-3-link", s),
            "profile"
        );
        assert_eq!(
            root_kind("/nix/var/nix/gcroots/hydra/abc-nixos-system.drv", s),
            "hydra"
        );
        assert_eq!(root_kind("/run/booted-system", s), "other");
        assert_eq!(root_kind("/home/b/src/x/result", s), "auto");
    }

    #[test]
    fn temp_roots_are_nul_separated() {
        let d = tempfile::tempdir().unwrap();
        write(
            &d.path().join("temproots/123"),
            "/nix/store/a-x\0/nix/store/b-y\0",
        );
        let mut t = temp_roots(&d.path().join("temproots"));
        t.sort();
        assert_eq!(t, vec!["/nix/store/a-x", "/nix/store/b-y"]);
        assert!(temp_roots(&d.path().join("nope")).is_empty());
    }
}
