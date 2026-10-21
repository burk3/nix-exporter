//! Helpers for building fake `/proc` and filesystem trees in tests.

use std::fs;
use std::path::Path;

pub fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A `/proc/<pid>` with status, cmdline, stat (start time 5000 ticks) and an empty fd dir.
/// Every process has 1000 kB resident.
pub fn fake_proc(proc_dir: &Path, pid: u32, ppid: u32, uid: u32, name: &str, argv: &[&str]) {
    let d = proc_dir.join(pid.to_string());
    write(
        &d.join("status"),
        &format!(
            "Name:\t{name}\nPPid:\t{ppid}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nVmRSS:\t    1000 kB\n"
        ),
    );
    let mut cmdline = argv.join("\0");
    cmdline.push('\0');
    write(&d.join("cmdline"), &cmdline);
    write(
        &d.join("stat"),
        &format!("{pid} ({name}) S {ppid} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5000 0"),
    );
    fs::create_dir_all(d.join("fd")).unwrap();
}

/// Makes `/proc/<pid>/fd/<fd>` a link to `socket:[inode]`.
pub fn fake_socket_fd(proc_dir: &Path, pid: u32, fd: u32, inode: u64) {
    std::os::unix::fs::symlink(
        format!("socket:[{inode}]"),
        proc_dir
            .join(pid.to_string())
            .join("fd")
            .join(fd.to_string()),
    )
    .unwrap();
}
