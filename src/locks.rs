//! Files with a lock held, from `/proc/locks`.

use std::collections::HashSet;
use std::path::Path;

/// `(dev, inode)` of every file with a lock held, where `dev` is in the form
/// of `MetadataExt::dev()`. Waiters (lines with `->`) are not holders.
pub fn held_files(text: &str) -> HashSet<(u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.get(1) == Some(&"->") {
                return None;
            }
            // MAJOR:MINOR:INODE, major and minor in hex, inode in decimal.
            let id = f.iter().find(|t| t.matches(':').count() == 2)?;
            let mut parts = id.split(':');
            let major = u32::from_str_radix(parts.next()?, 16).ok()?;
            let minor = u32::from_str_radix(parts.next()?, 16).ok()?;
            let inode = parts.next()?.parse().ok()?;
            Some((libc::makedev(major, minor), inode))
        })
        .collect()
}

/// The device `/proc/locks` reports for files under `path`: the
/// `MAJOR:MINOR` of the mount containing it, from `/proc/self/mountinfo`.
/// This is the superblock's device, which is not always `stat`'s `st_dev`:
/// btrfs reports a per-subvolume device from `stat`.
pub fn mount_dev(mountinfo: &str, path: &Path) -> Option<u64> {
    let mut best: Option<(usize, u64)> = None;
    for line in mountinfo.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(dev), Some(point)) = (f.get(2), f.get(4)) else {
            continue;
        };
        let point = unescape(point);
        if !path.starts_with(&point) {
            continue;
        }
        let Some((major, minor)) = dev.split_once(':') else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) else {
            continue;
        };
        let len = point.len();
        // Later lines are mounted on top of earlier ones at the same point.
        if best.is_none_or(|(l, _)| len >= l) {
            best = Some((len, libc::makedev(major, minor)));
        }
    }
    best.map(|(_, dev)| dev)
}

/// Undoes mountinfo's octal escapes (`\040` for a space, etc.).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_holders_only() {
        let text = "1: FLOCK  ADVISORY  WRITE 819892 00:2a:20118213 0 EOF\n2: POSIX  ADVISORY  READ 1234 00:1b:555 0 EOF\n2: -> POSIX  ADVISORY  WRITE 99 00:1b:777 0 EOF\n3: FLOCK  ADVISORY  WRITE 5 103:1f:42 0 EOF\n";
        let held = held_files(text);
        assert!(held.contains(&(libc::makedev(0, 0x2a), 20118213)));
        assert!(held.contains(&(libc::makedev(0, 0x1b), 555)));
        assert!(held.contains(&(libc::makedev(0x103, 0x1f), 42)));
        assert!(!held.contains(&(libc::makedev(0, 0x1b), 777)));
        // Same inode, other device: not held.
        assert!(!held.contains(&(libc::makedev(0, 0x2b), 20118213)));
        assert_eq!(held.len(), 3);
    }

    #[test]
    fn finds_the_mount_device() {
        let info = "22 1 0:33 /root / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw\n\
                    23 22 0:33 /nix /nix rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw\n\
                    24 23 0:40 / /nix/var/nix rw shared:3 - tmpfs tmpfs rw\n\
                    25 22 259:3 / /mnt/a\\040b rw shared:4 - ext4 /dev/x rw\n";
        let dev = |p: &str| mount_dev(info, Path::new(p));
        assert_eq!(dev("/nix/var/nix/current-load"), Some(libc::makedev(0, 40)));
        assert_eq!(dev("/nix/store"), Some(libc::makedev(0, 33)));
        assert_eq!(dev("/nix/variant"), Some(libc::makedev(0, 33)));
        assert_eq!(dev("/mnt/a b/c"), Some(libc::makedev(259, 3)));
        assert_eq!(mount_dev("", Path::new("/x")), None);
    }
}
