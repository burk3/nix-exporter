//! Filesystem locations, overridable so tests can point at fixture trees.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    pub proc_dir: PathBuf,
    pub cgroup_root: PathBuf,
    pub state_dir: PathBuf,
    pub etc_dir: PathBuf,
    pub store_dir: String,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            proc_dir: "/proc".into(),
            cgroup_root: "/sys/fs/cgroup".into(),
            state_dir: "/nix/var/nix".into(),
            etc_dir: "/etc".into(),
            store_dir: "/nix/store".into(),
        }
    }
}

impl Paths {
    pub fn db(&self) -> PathBuf {
        self.state_dir.join("db/db.sqlite")
    }

    pub fn active_builds(&self) -> PathBuf {
        self.state_dir.join("active-builds")
    }

    pub fn current_load(&self) -> PathBuf {
        self.state_dir.join("current-load")
    }

    pub fn temproots(&self) -> PathBuf {
        self.state_dir.join("temproots")
    }
}
