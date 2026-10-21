//! User and group names from /etc/passwd and /etc/group.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Users {
    names: HashMap<u32, String>,
    uids: HashMap<String, u32>,
    primary_gid: HashMap<u32, u32>,
    groups: HashMap<String, (u32, Vec<String>)>,
}

impl Users {
    pub fn read(etc_dir: &Path) -> Self {
        let passwd = fs::read_to_string(etc_dir.join("passwd")).unwrap_or_default();
        let group = fs::read_to_string(etc_dir.join("group")).unwrap_or_default();
        Self::parse(&passwd, &group)
    }

    pub fn parse(passwd: &str, group: &str) -> Self {
        let mut u = Users::default();
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 {
                continue;
            }
            if let (Ok(uid), Ok(gid)) = (f[2].parse::<u32>(), f[3].parse::<u32>()) {
                u.names.insert(uid, f[0].to_string());
                u.uids.insert(f[0].to_string(), uid);
                u.primary_gid.insert(uid, gid);
            }
        }
        for line in group.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 {
                continue;
            }
            if let Ok(gid) = f[2].parse::<u32>() {
                let members = f[3]
                    .split(',')
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect();
                u.groups.insert(f[0].to_string(), (gid, members));
            }
        }
        u
    }

    /// The user name for `uid`, or the number when it has none.
    pub fn name(&self, uid: u32) -> String {
        self.names
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    /// UIDs in `group`: listed members plus users whose primary group it is.
    pub fn group_member_uids(&self, group: &str) -> HashSet<u32> {
        let Some((gid, members)) = self.groups.get(group) else {
            return HashSet::new();
        };
        let mut out: HashSet<u32> = members
            .iter()
            .filter_map(|m| self.uids.get(m).copied())
            .collect();
        out.extend(
            self.primary_gid
                .iter()
                .filter(|(_, g)| *g == gid)
                .map(|(uid, _)| *uid),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/bin/sh\nburke:x:1000:100::/home/burke:/bin/zsh\nnixbld1:x:30001:30000::/var/empty:/bin/false\nnixbld2:x:30002:65534::/var/empty:/bin/false\n";
    const GROUP: &str = "root:x:0:\nusers:x:100:\nnixbld:x:30000:nixbld2\n";

    #[test]
    fn names_and_group_members() {
        let u = Users::parse(PASSWD, GROUP);
        assert_eq!(u.name(1000), "burke");
        assert_eq!(u.name(872415232), "872415232");
        let members = u.group_member_uids("nixbld");
        assert!(members.contains(&30001), "primary group");
        assert!(members.contains(&30002), "listed member");
        assert!(!members.contains(&1000));
        assert!(u.group_member_uids("missing").is_empty());
    }
}
