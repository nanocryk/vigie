//! Folders of scratch sessions, named by the local date and two random words.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use uuid::Uuid;

const ADJECTIVES: [&str; 32] = [
    "amber", "bold", "brisk", "calm", "clever", "coral", "crisp", "dusty", "eager", "fancy",
    "gentle", "golden", "happy", "hazy", "jolly", "keen", "lively", "lucky", "mellow", "misty",
    "nimble", "proud", "quiet", "rapid", "rusty", "shy", "silver", "sunny", "swift", "tidy",
    "vivid", "witty",
];

const NOUNS: [&str; 32] = [
    "badger", "beaver", "bison", "crane", "falcon", "ferret", "finch", "fox", "gecko", "heron",
    "ibis", "koala", "lark", "lemur", "lynx", "marten", "moose", "newt", "otter", "owl", "panda",
    "pika", "puffin", "quail", "raven", "robin", "seal", "stoat", "swan", "tapir", "walrus",
    "wren",
];

/// Names tried before giving up; a clash needs the same date and both words.
const ATTEMPTS: usize = 16;

/// `2026-09-29-amber-fox`: the date then one word of each list, picked by `seed`.
pub fn folder_name(date: NaiveDate, seed: [u8; 2]) -> String {
    let adjective = ADJECTIVES[usize::from(seed[0]) % ADJECTIVES.len()];
    let noun = NOUNS[usize::from(seed[1]) % NOUNS.len()];
    format!("{}-{adjective}-{noun}", date.format("%Y-%m-%d"))
}

/// Creates a new folder in `root`, and `root` if missing; returns its path.
pub fn create_folder(root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
    let date = Local::now().date_naive();
    for _ in 0..ATTEMPTS {
        let random = Uuid::new_v4().into_bytes();
        let path = root.join(folder_name(date, [random[0], random[1]]));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
        }
    }
    bail!(
        "no free scratch folder name in {} after {ATTEMPTS} tries",
        root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_name_is_the_date_then_two_words() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 29).expect("date");
        assert_eq!(folder_name(date, [0, 7]), "2026-09-29-amber-fox");
        assert_eq!(folder_name(date, [32, 39]), "2026-09-29-amber-fox");
    }

    #[test]
    fn create_folder_makes_the_root_and_a_new_folder_each_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("scratch");
        let first = create_folder(&root).expect("first");
        let second = create_folder(&root).expect("second");
        assert_ne!(first, second);
        assert!(first.is_dir() && second.is_dir());
        assert_eq!(first.parent(), Some(root.as_path()));
    }
}
