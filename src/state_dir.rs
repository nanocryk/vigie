//! `~/.vigie`, where the server keeps its database, keys and device registry.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home dir")?;
    Ok(home.join(".vigie"))
}

/// Creates `~/.vigie` if missing and makes it reachable by the owner only, which
/// also covers what an older version created inside it with looser modes.
pub fn create() -> Result<PathBuf> {
    let dir = path()?;
    create_private_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod {}", dir.display()))?;
    }
    Ok(dir)
}

/// Creates `path` and its missing parents, each reachable by the owner only.
pub fn create_private_dir(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("mkdir {}", path.display()))
}

/// Options whose `create` makes a file readable by the owner only from the start.
pub fn private_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Writes `contents` to `path`, readable by the owner only.
pub fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .and_then(|mut file| file.write_all(contents.as_ref()))
        .with_context(|| format!("write {}", path.display()))?;
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dirs_and_files_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a").join("b");
        create_private_dir(&dir).unwrap();
        assert_eq!(mode(&tmp.path().join("a")), 0o700);
        assert_eq!(mode(&dir), 0o700);
        let file = dir.join("key");
        write_private(&file, b"secret").unwrap();
        assert_eq!(mode(&file), 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"again").unwrap();
        assert_eq!(mode(&file), 0o600);
    }
}
