//! The `cli_catalog` table: what the CLI offers every discussion.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::Store;
use crate::claude::protocol::AppliedSettings;
use crate::session::{CommandInfo, ModelChoice};

/// What the CLI offers every discussion, as the latest Claude start reported it;
/// a field is `None` while nothing reported it yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SharedCatalog {
    pub models: Option<Vec<ModelChoice>>,
    pub commands: Option<Vec<CommandInfo>>,
    /// Commands that only work in the terminal, from a `system/init` line.
    pub terminal_commands: Option<Vec<String>>,
    /// What the CLI applies with no model and no effort picked.
    pub defaults: Option<AppliedSettings>,
}

const MODELS_KEY: &str = "models";
const COMMANDS_KEY: &str = "commands";
const TERMINAL_COMMANDS_KEY: &str = "terminal_commands";
const DEFAULTS_KEY: &str = "defaults";

impl Store {
    pub async fn shared_catalog(&self) -> Result<SharedCatalog> {
        self.call(|connection| {
            Ok(SharedCatalog {
                models: catalog_entry(connection, MODELS_KEY)?,
                commands: catalog_entry(connection, COMMANDS_KEY)?,
                terminal_commands: catalog_entry(connection, TERMINAL_COMMANDS_KEY)?,
                defaults: catalog_entry(connection, DEFAULTS_KEY)?,
            })
        })
        .await
    }

    /// Stores the models and commands of an `initialize` reply as the shared
    /// ones; resolves to whether that changed them.
    pub async fn record_catalog(
        &self,
        models: &[ModelChoice],
        commands: &[CommandInfo],
    ) -> Result<bool> {
        self.set_catalog_entries(vec![
            (MODELS_KEY, serde_json::to_string(models)?),
            (COMMANDS_KEY, serde_json::to_string(commands)?),
        ])
        .await
    }

    /// Resolves to whether that changed the stored ones.
    pub async fn record_terminal_commands(&self, commands: &[String]) -> Result<bool> {
        self.set_catalog_entries(vec![(
            TERMINAL_COMMANDS_KEY,
            serde_json::to_string(commands)?,
        )])
        .await
    }

    /// Stores what the CLI applies with no model and no effort picked; resolves
    /// to whether that changed the stored values.
    pub async fn record_default_settings(&self, applied: &AppliedSettings) -> Result<bool> {
        self.set_catalog_entries(vec![(DEFAULTS_KEY, serde_json::to_string(applied)?)])
            .await
    }

    async fn set_catalog_entries(&self, entries: Vec<(&'static str, String)>) -> Result<bool> {
        self.call(move |connection| {
            let mut changed = false;
            for (key, value) in &entries {
                let written = connection.execute(
                    "INSERT INTO cli_catalog (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value
                     WHERE value IS NOT excluded.value",
                    params![key, value],
                )?;
                changed |= written > 0;
            }
            Ok(changed)
        })
        .await
    }
}

fn catalog_entry<T: DeserializeOwned>(connection: &Connection, key: &str) -> Result<Option<T>> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM cli_catalog WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| serde_json::from_str(&value).with_context(|| format!("cli_catalog {key}")))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_shared_catalog_survives_a_reopen_and_reports_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.shared_catalog().await.unwrap(),
            SharedCatalog::default()
        );
        let models = vec![ModelChoice {
            value: "opus".into(),
            resolved_model: Some("claude-opus-5-5".into()),
            display_name: "Opus 5.5".into(),
            description: None,
            effort_levels: vec!["low".into(), "medium".into()],
        }];
        let commands = vec![CommandInfo {
            name: "review".into(),
            description: "Review a change".into(),
            argument_hint: String::new(),
        }];
        let defaults = AppliedSettings {
            model: Some("claude-opus-5-5[1m]".into()),
            effort: Some("medium".into()),
        };
        let terminal = vec!["doctor".to_string()];
        assert!(store.record_catalog(&models, &commands).await.unwrap());
        assert!(!store.record_catalog(&models, &commands).await.unwrap());
        assert!(store.record_default_settings(&defaults).await.unwrap());
        assert!(!store.record_default_settings(&defaults).await.unwrap());
        assert!(store.record_terminal_commands(&terminal).await.unwrap());
        assert!(!store.record_terminal_commands(&terminal).await.unwrap());
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.shared_catalog().await.unwrap(),
            SharedCatalog {
                models: Some(models),
                commands: Some(commands.clone()),
                terminal_commands: Some(terminal),
                defaults: Some(defaults),
            }
        );
        assert!(store.record_catalog(&[], &commands).await.unwrap());
        assert_eq!(store.shared_catalog().await.unwrap().models, Some(vec![]));
    }
}
