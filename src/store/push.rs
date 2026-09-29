//! The `push_subscriptions` table: one Web Push subscription per device.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;
use crate::http::push::{PushKeys, PushSubscription};

impl Store {
    /// Every stored subscription, by device id.
    pub async fn push_subscriptions(&self) -> Result<Vec<(String, PushSubscription)>> {
        self.call(|connection| {
            let mut statement = connection
                .prepare("SELECT device_id, endpoint, p256dh, auth FROM push_subscriptions")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get("device_id")?, read_subscription(row)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn push_subscription(&self, device: &str) -> Result<Option<PushSubscription>> {
        let device = device.to_string();
        self.call(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT endpoint, p256dh, auth FROM push_subscriptions WHERE device_id = ?1",
                    [device],
                    read_subscription,
                )
                .optional()?)
        })
        .await
    }

    /// Replaces the device's subscription.
    pub async fn set_push_subscription(
        &self,
        device: &str,
        subscription: &PushSubscription,
    ) -> Result<()> {
        let (device, subscription) = (device.to_string(), subscription.clone());
        self.call(move |connection| {
            connection.execute(
                "INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (device_id) DO UPDATE
                     SET endpoint = ?2, p256dh = ?3, auth = ?4",
                params![
                    device,
                    subscription.endpoint,
                    subscription.keys.p256dh,
                    subscription.keys.auth
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn remove_push_subscription(&self, device: &str) -> Result<()> {
        let device = device.to_string();
        self.call(move |connection| {
            connection.execute(
                "DELETE FROM push_subscriptions WHERE device_id = ?1",
                [device],
            )?;
            Ok(())
        })
        .await
    }
}

/// A `push_subscriptions` row, read by column name.
fn read_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<PushSubscription> {
    Ok(PushSubscription {
        endpoint: row.get("endpoint")?,
        keys: PushKeys {
            p256dh: row.get("p256dh")?,
            auth: row.get("auth")?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription(endpoint: &str) -> PushSubscription {
        PushSubscription {
            endpoint: endpoint.into(),
            keys: PushKeys {
                p256dh: "key".into(),
                auth: "secret".into(),
            },
        }
    }

    #[tokio::test]
    async fn a_device_holds_one_subscription_until_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        assert_eq!(store.push_subscription("phone").await.unwrap(), None);
        store
            .set_push_subscription("phone", &subscription("https://a"))
            .await
            .unwrap();
        store
            .set_push_subscription("phone", &subscription("https://b"))
            .await
            .unwrap();
        store
            .set_push_subscription("laptop", &subscription("https://c"))
            .await
            .unwrap();
        assert_eq!(
            store.push_subscription("phone").await.unwrap(),
            Some(subscription("https://b"))
        );
        let mut all = store.push_subscriptions().await.unwrap();
        all.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            all,
            [
                ("laptop".to_string(), subscription("https://c")),
                ("phone".to_string(), subscription("https://b")),
            ]
        );
        store.remove_push_subscription("phone").await.unwrap();
        assert_eq!(store.push_subscription("phone").await.unwrap(), None);
        assert_eq!(store.push_subscriptions().await.unwrap().len(), 1);
    }
}
