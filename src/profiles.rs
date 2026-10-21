//! Profile generation links.

use std::path::Path;

/// For `<dir>/<name>-<N>-link`, the profile path `<dir>/<name>` and generation `N`.
pub fn generation_link(link: &str) -> Option<(String, u64)> {
    let p = Path::new(link);
    let file = p.file_name()?.to_str()?;
    let (name, n) = file.strip_suffix("-link")?.rsplit_once('-')?;
    let n: u64 = n.parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((p.with_file_name(name).to_string_lossy().into_owned(), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generation_links() {
        assert_eq!(
            generation_link("/nix/var/nix/profiles/system-55-link"),
            Some(("/nix/var/nix/profiles/system".to_string(), 55))
        );
        assert_eq!(
            generation_link("/home/b/.local/state/nix/profiles/home-manager-12-link"),
            Some((
                "/home/b/.local/state/nix/profiles/home-manager".to_string(),
                12
            ))
        );
        assert_eq!(generation_link("/nix/var/nix/profiles/system"), None);
        assert_eq!(generation_link("/x/foo-link"), None);
        assert_eq!(generation_link("/x/a-b-link"), None);
        assert_eq!(generation_link("/x/-3-link"), None);
    }
}
