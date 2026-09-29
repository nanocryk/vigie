//! `POST /api/sessions/{id}/upload`: a file attached to a message, kept in the
//! session's uploads folder.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
};
use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::http::api::ApiError;

pub const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

#[derive(Deserialize)]
pub struct UploadQuery {
    /// The file's name on the client, reduced to a safe base name before use.
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct Uploaded {
    pub path: String,
}

/// Stores the body as `{random}-{name}`: uploads of one name never collide, and
/// the extension stays for Claude to recognize the file type.
pub async fn upload_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<Uploaded>, ApiError> {
    if body.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "empty_body"));
    }
    let known = state
        .manager
        .store()
        .session(id)
        .await
        .map_err(|err| ApiError::internal("find session", err))?;
    if known.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_session"));
    }
    let dir = state.manager.uploads_dir(id);
    let path = dir.join(format!("{}-{}", random_stem(), safe_name(&query.name)));
    let written = path.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&written, &body)
    })
    .await
    .map_err(|err| ApiError::internal("upload task", err))?
    .map_err(|err| ApiError::internal("write upload", err))?;
    Ok(Json(Uploaded {
        path: path.to_string_lossy().into_owned(),
    }))
}

fn random_stem() -> String {
    let mut bytes = [0u8; 6];
    SysRng
        .try_fill_bytes(&mut bytes)
        .expect("OS random source unavailable");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The last path part of `name`, without control characters, with the ones
/// Windows forbids replaced, and trimmed of dots and spaces.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    let trimmed =
        cleaned.trim_matches(|character: char| character == '.' || character.is_whitespace());
    if trimmed.is_empty() {
        "upload.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_keeps_the_base_name_only() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\Windows\\evil.png"), "evil.png");
        assert_eq!(safe_name("photo.jpeg"), "photo.jpeg");
    }

    #[test]
    fn safe_name_replaces_what_windows_forbids() {
        assert_eq!(safe_name("a<b>c:d\"e|f?g*h.txt"), "a_b_c_d_e_f_g_h.txt");
        assert_eq!(safe_name("a\u{7}b.txt"), "ab.txt");
    }

    #[test]
    fn an_empty_name_falls_back() {
        assert_eq!(safe_name(""), "upload.bin");
        assert_eq!(safe_name("..."), "upload.bin");
        assert_eq!(safe_name("dir/"), "upload.bin");
    }

    #[tokio::test]
    async fn an_upload_lands_in_its_sessions_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::for_tests(
            tmp.path(),
            crate::claude::child::Program::claude(),
            "device",
            0,
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let store = state.manager.store();
        store.session_started(id, tmp.path(), chrono::Utc::now(), None);
        store.flush().await.unwrap();
        let upload = |id, name: &str, body: &'static str| {
            upload_route(
                State(state.clone()),
                Path(id),
                Query(UploadQuery { name: name.into() }),
                Bytes::from(body),
            )
        };

        let Json(uploaded) = upload(id, "../shot.png", "png bytes").await.unwrap();
        let path = std::path::Path::new(&uploaded.path);
        assert_eq!(
            path.parent(),
            Some(tmp.path().join("uploads").join(id.to_string()).as_path())
        );
        assert!(uploaded.path.ends_with("-shot.png"), "{}", uploaded.path);
        assert_eq!(std::fs::read(path).unwrap(), b"png bytes");

        let unknown = upload(Uuid::new_v4(), "a.txt", "x").await.unwrap_err();
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        let empty = upload(id, "a.txt", "").await.unwrap_err();
        assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn stems_are_twelve_hex_digits() {
        let stem = random_stem();
        assert_eq!(stem.len(), 12);
        assert!(stem.chars().all(|character| character.is_ascii_hexdigit()));
    }
}
