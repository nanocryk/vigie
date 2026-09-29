//! Read-mostly filesystem routes behind the file explorer: listing, places,
//! file content, image bytes and folder creation. Sessions already reach the
//! whole machine, so browsing is not confined to a root.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::Query,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::ApiError;

/// Text past this is cut and flagged truncated.
pub const TEXT_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Largest image served inline, so one tap cannot pull a huge asset over a phone link.
pub const IMAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// A NUL byte in this prefix marks a file as binary.
const SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug)]
pub enum FsError {
    NotFound,
    NotADirectory,
    NotAFile,
    BadName(&'static str),
    AlreadyExists,
    NotAnImage,
    TooLarge,
    Io(io::Error),
}

impl From<io::Error> for FsError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            _ => Self::Io(err),
        }
    }
}

impl From<FsError> for ApiError {
    fn from(err: FsError) -> Self {
        match err {
            FsError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found"),
            FsError::NotADirectory => Self::new(StatusCode::BAD_REQUEST, "not_a_directory"),
            FsError::NotAFile => Self::new(StatusCode::BAD_REQUEST, "not_a_file"),
            FsError::BadName(reason) => Self::new(StatusCode::BAD_REQUEST, reason),
            FsError::AlreadyExists => Self::new(StatusCode::CONFLICT, "already_exists"),
            FsError::NotAnImage => Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "not_an_image"),
            FsError::TooLarge => Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            FsError::Io(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                Self::new(StatusCode::FORBIDDEN, "permission_denied")
            }
            FsError::Io(err) => Self::internal("filesystem route failed", err),
        }
    }
}

impl IntoResponse for FsError {
    fn into_response(self) -> Response {
        ApiError::from(self).into_response()
    }
}

/// The path as users write it: Windows' verbatim prefix from `canonicalize` removed.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        local.to_string()
    } else {
        text.into_owned()
    }
}

/// `raw` resolved, links followed. A relative path is not found: there is no
/// working folder to resolve it against.
pub fn canonical(raw: &str) -> io::Result<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(io::ErrorKind::NotFound.into());
    }
    std::fs::canonicalize(path)
}

/// A NUL byte in the first `SNIFF_BYTES` marks binary content.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Files only.
    pub size: Option<u64>,
    pub modified: Option<DateTime<Utc>>,
    /// A dotfile, or carrying the Windows hidden attribute.
    pub hidden: bool,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

#[cfg(windows)]
fn hidden_attribute(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn hidden_attribute(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Subfolders then files, each group sorted case-insensitively. Entries whose
/// name is not UTF-8 or whose link target is gone are skipped.
pub fn list(raw: &str) -> Result<Listing, FsError> {
    let dir = canonical(raw)?;
    if !dir.is_dir() {
        return Err(FsError::NotADirectory);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(link_meta) = entry.metadata() else {
            continue;
        };
        // Follows links, so a link to a folder lists as a folder.
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let kind = if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        entries.push(Entry {
            hidden: name.starts_with('.') || hidden_attribute(&link_meta),
            size: (kind == EntryKind::File).then_some(meta.len()),
            modified: meta.modified().ok().map(DateTime::<Utc>::from),
            kind,
            name,
        });
    }
    entries.sort_by(|left, right| {
        (left.kind != EntryKind::Dir, left.name.to_lowercase())
            .cmp(&(right.kind != EntryKind::Dir, right.name.to_lowercase()))
    });
    Ok(Listing {
        path: display_path(&dir),
        parent: dir.parent().map(display_path),
        entries,
    })
}

#[derive(Debug, Serialize)]
pub struct Place {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct Places {
    pub drives: Vec<Place>,
    pub folders: Vec<Place>,
}

#[cfg(windows)]
fn drives() -> Vec<Place> {
    (b'A'..=b'Z')
        .map(char::from)
        .map(|letter| (format!("{letter}:"), format!(r"{letter}:\")))
        .filter(|(_, root)| Path::new(root).is_dir())
        .map(|(label, path)| Place { label, path })
        .collect()
}

#[cfg(not(windows))]
fn drives() -> Vec<Place> {
    vec![Place {
        label: "/".to_string(),
        path: "/".to_string(),
    }]
}

pub fn places() -> Places {
    let common = [
        ("Home", dirs::home_dir()),
        ("Documents", dirs::document_dir()),
        ("Desktop", dirs::desktop_dir()),
        ("Downloads", dirs::download_dir()),
    ];
    let folders = common
        .into_iter()
        .filter_map(|(label, path)| path.filter(|path| path.is_dir()).map(|path| (label, path)))
        .map(|(label, path)| Place {
            label: label.to_string(),
            path: display_path(&path),
        })
        .collect();
    Places {
        drives: drives(),
        folders,
    }
}

/// Image types a browser decodes. SVG is absent: it is text and shows as source.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Content {
    Text { text: String, truncated: bool },
    Image,
    Binary,
}

#[derive(Debug, Serialize)]
pub struct FileView {
    pub path: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub content: Content,
}

fn guarded_file(raw: &str) -> Result<(PathBuf, std::fs::Metadata), FsError> {
    let path = canonical(raw)?;
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() {
        return Err(FsError::NotAFile);
    }
    Ok((path, meta))
}

/// Decodes `bytes` as text, or `None` for binary. A cut at the read cap may
/// split the last character, which is dropped rather than failing the file.
fn decode_text(mut bytes: Vec<u8>, cut: bool) -> Option<String> {
    if is_binary(&bytes) {
        return None;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(err) if cut && err.utf8_error().error_len().is_none() => {
            let valid = err.utf8_error().valid_up_to();
            bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).ok()
        }
        Err(_) => None,
    }
}

pub fn read_file(raw: &str) -> Result<FileView, FsError> {
    let (path, meta) = guarded_file(raw)?;
    let size = meta.len();
    let content = if image_mime(&path).is_some() {
        Content::Image
    } else {
        let mut bytes = Vec::new();
        std::fs::File::open(&path)?
            .take(TEXT_MAX_BYTES)
            .read_to_end(&mut bytes)?;
        let truncated = size > TEXT_MAX_BYTES;
        match decode_text(bytes, truncated) {
            Some(text) => Content::Text { text, truncated },
            None => Content::Binary,
        }
    };
    Ok(FileView {
        path: display_path(&path),
        size,
        modified: meta.modified().ok().map(DateTime::<Utc>::from),
        content,
    })
}

/// Creates `name` inside `parent` and returns the new folder's path.
pub fn make_dir(parent: &str, name: &str) -> Result<String, FsError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(FsError::BadName("empty_name"));
    }
    if name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(FsError::BadName("bad_name"));
    }
    let parent = canonical(parent)?;
    if !parent.is_dir() {
        return Err(FsError::NotADirectory);
    }
    let path = parent.join(name);
    std::fs::create_dir(&path)?;
    Ok(display_path(&path))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, FsError> + Send + 'static,
) -> Result<T, FsError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| FsError::Io(io::Error::other(err)))?
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

pub async fn list_route(Query(query): Query<PathQuery>) -> Result<Json<Listing>, FsError> {
    blocking(move || list(&query.path)).await.map(Json)
}

pub async fn places_route() -> Result<Json<Places>, FsError> {
    blocking(|| Ok(places())).await.map(Json)
}

pub async fn file_route(Query(query): Query<PathQuery>) -> Result<Json<FileView>, FsError> {
    blocking(move || read_file(&query.path)).await.map(Json)
}

pub async fn raw_route(Query(query): Query<PathQuery>) -> Result<Response, FsError> {
    let (mime, bytes) = blocking(move || {
        let (path, meta) = guarded_file(&query.path)?;
        let mime = image_mime(&path).ok_or(FsError::NotAnImage)?;
        if meta.len() > IMAGE_MAX_BYTES {
            return Err(FsError::TooLarge);
        }
        Ok((mime, std::fs::read(&path)?))
    })
    .await?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
            // The file can change under the viewer.
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct MakeDir {
    pub parent: String,
    pub name: String,
}

pub async fn mkdir_route(Json(request): Json<MakeDir>) -> Result<Response, FsError> {
    let path = blocking(move || make_dir(&request.parent, &request.name)).await?;
    Ok((StatusCode::CREATED, Json(json!({ "path": path }))).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(view: &FileView) -> (&str, bool) {
        match &view.content {
            Content::Text { text, truncated } => (text.as_str(), *truncated),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn list_puts_folders_first_and_flags_dotfiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join("zeta")).expect("mkdir");
        std::fs::create_dir(tmp.path().join("Alpha")).expect("mkdir");
        std::fs::write(tmp.path().join("b.txt"), "hi").expect("write");
        std::fs::write(tmp.path().join(".env"), "x").expect("write");

        let listing = list(tmp.path().to_str().expect("utf-8 path")).expect("list");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", ".env", "b.txt"]);
        assert_eq!(listing.entries[3].size, Some(2));
        assert_eq!(listing.entries[0].size, None);
        assert!(listing.entries[2].hidden);
        assert!(!listing.entries[3].hidden);
        assert!(!listing.path.starts_with(r"\\?\"), "verbatim prefix leaked");
        assert!(listing.parent.is_some());
    }

    #[test]
    fn list_refuses_relative_paths_and_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "x").expect("write");
        assert!(matches!(list("relative"), Err(FsError::NotFound)));
        assert!(matches!(
            list(file.to_str().expect("utf-8 path")),
            Err(FsError::NotADirectory)
        ));
    }

    #[test]
    fn read_file_tells_text_image_and_binary_apart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let text = tmp.path().join("a.md");
        let image = tmp.path().join("b.PNG");
        let binary = tmp.path().join("c.bin");
        std::fs::write(&text, "# title").expect("write");
        std::fs::write(&image, [0x89, b'P', b'N', b'G']).expect("write");
        std::fs::write(&binary, [1, 0, 2]).expect("write");

        let view = read_file(text.to_str().expect("utf-8 path")).expect("text");
        assert_eq!(text_of(&view), ("# title", false));
        assert_eq!(
            read_file(image.to_str().expect("utf-8 path"))
                .expect("image")
                .content,
            Content::Image
        );
        assert_eq!(
            read_file(binary.to_str().expect("utf-8 path"))
                .expect("binary")
                .content,
            Content::Binary
        );
    }

    #[test]
    fn a_cut_through_a_character_drops_it_instead_of_failing() {
        let mut bytes = "é".repeat(4).into_bytes();
        bytes.pop();
        assert_eq!(decode_text(bytes.clone(), true).as_deref(), Some("ééé"));
        assert_eq!(decode_text(bytes, false), None);
    }

    #[test]
    fn make_dir_creates_and_refuses_bad_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().to_str().expect("utf-8 path");
        let created = make_dir(parent, " new ").expect("mkdir");
        assert!(Path::new(&created).is_dir());
        assert!(created.ends_with("new"));
        assert!(matches!(
            make_dir(parent, "new"),
            Err(FsError::AlreadyExists)
        ));
        for bad in ["", "  ", ".", "..", "a/b", r"a\b"] {
            assert!(
                matches!(make_dir(parent, bad), Err(FsError::BadName(_))),
                "accepted {bad:?}"
            );
        }
    }
}
