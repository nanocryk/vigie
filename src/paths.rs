//! Checks on paths that come from clients or from Claude, before they reach
//! the filesystem.

use std::path::Path;

/// Whether `path` names another machine: a Windows UNC or device path
/// (`\\host\share`, `//host/share`, `\\?\UNC\...`, `\\.\...`). Touching one
/// opens an SMB connection, and Windows answers its challenge with the
/// user's NTLM hash. Always false off Windows, where `//` is a local path.
pub fn is_network_path(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    cfg!(windows) && (text.starts_with(r"\\") || text.starts_with("//"))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn unc_and_device_paths_are_network_paths() {
        for path in [
            r"\\attacker.example\s\x.png",
            "//attacker.example/s/x.png",
            r"\\?\UNC\attacker.example\s",
            r"\\.\pipe\x",
        ] {
            assert!(is_network_path(Path::new(path)), "{path}");
        }
        for path in [r"C:\Users\me\x.png", "C:/x", r"relative\x", r"\rooted"] {
            assert!(!is_network_path(Path::new(path)), "{path}");
        }
    }
}
