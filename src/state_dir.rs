//! `~/.vigie`, where the server keeps its database, keys and device registry.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".vigie"))
}

/// Writes `contents` to `path`, readable by the owner only.
pub fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    restrict_to_owner(path)
}

/// Makes `path` readable by the owner only; a no-op off Unix.
pub fn restrict_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
