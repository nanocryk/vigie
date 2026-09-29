//! The `plans` table: the plan text each file last showed for review.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;

impl Store {
    /// Stores `text` as the last reviewed text of the plan at `path`; resolves to
    /// the text it replaces.
    pub async fn swap_plan(&self, path: String, text: String) -> Result<Option<String>> {
        self.call(move |connection| {
            let previous: Option<String> = connection
                .query_row("SELECT text FROM plans WHERE path = ?1", [&path], |row| {
                    row.get(0)
                })
                .optional()?;
            connection.execute(
                "INSERT INTO plans (path, text) VALUES (?1, ?2)
                 ON CONFLICT (path) DO UPDATE SET text = excluded.text",
                params![path, text],
            )?;
            Ok(previous)
        })
        .await
    }

    /// Drops the reviewed text of the plan at `path`, so its next review has none.
    pub fn forget_plan(&self, path: String) {
        self.write("forget plan", move |connection| {
            connection.execute("DELETE FROM plans WHERE path = ?1", [path])?;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_swap_returns_the_text_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.swap_plan("a.md".into(), "one".into()).await.unwrap(),
            None
        );
        assert_eq!(
            store
                .swap_plan("b.md".into(), "other".into())
                .await
                .unwrap(),
            None
        );
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.swap_plan("a.md".into(), "two".into()).await.unwrap(),
            Some("one".into())
        );
        assert_eq!(
            store
                .swap_plan("a.md".into(), "three".into())
                .await
                .unwrap(),
            Some("two".into())
        );
        store.forget_plan("a.md".into());
        assert_eq!(
            store.swap_plan("a.md".into(), "four".into()).await.unwrap(),
            None
        );
    }
}
