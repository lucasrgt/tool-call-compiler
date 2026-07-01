//! Filesystem tool implementations.

use std::path::PathBuf;

use globset::Glob;
use serde_json::{Value, json};

use crate::{FsError, FsExecutor, required_str};

pub(crate) async fn read_file(executor: &FsExecutor, input: &Value) -> Result<Value, FsError> {
    let path = executor.resolve(required_str(input, "path")?)?;
    let max_bytes = executor.max_read_bytes(input);
    let bytes = tokio::fs::read(path).await?;

    if bytes.len() <= max_bytes {
        return Ok(json!({ "content": String::from_utf8_lossy(&bytes) }));
    }

    let total = bytes.len();
    let mut cut = max_bytes;
    // Do not split a UTF-8 code point at the boundary.
    while cut > 0 && !bytes.is_char_boundary_at(cut) {
        cut -= 1;
    }
    Ok(json!({
        "content": String::from_utf8_lossy(&bytes[..cut]),
        "truncated": true,
        "bytes": total,
    }))
}

trait CharBoundary {
    fn is_char_boundary_at(&self, index: usize) -> bool;
}

impl CharBoundary for Vec<u8> {
    fn is_char_boundary_at(&self, index: usize) -> bool {
        if index == 0 || index >= self.len() {
            return true;
        }
        // A UTF-8 continuation byte is 0b10xxxxxx.
        (self[index] & 0b1100_0000) != 0b1000_0000
    }
}

pub(crate) async fn write_file(executor: &FsExecutor, input: &Value) -> Result<Value, FsError> {
    let path = executor.resolve(required_str(input, "path")?)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, required_str(input, "content")?).await?;
    Ok(json!({ "path": input["path"], "written": true }))
}

/// Transactional multi-file write: every path is resolved before anything is
/// written; existing files are backed up in memory; on any failure the
/// already-written files are restored (or removed when they did not exist).
pub(crate) async fn write_files(executor: &FsExecutor, input: &Value) -> Result<Value, FsError> {
    let files = input
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| FsError::MissingField("files".into()))?;

    let mut staged: Vec<(String, PathBuf, String)> = Vec::with_capacity(files.len());
    for file in files {
        let raw = required_str(file, "path")?;
        let content = required_str(file, "content")?;
        let path = executor.resolve(raw)?;
        staged.push((raw.to_owned(), path, content.to_owned()));
    }

    let mut backups: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::with_capacity(staged.len());
    for (raw, path, content) in &staged {
        let backup = match tokio::fs::read(path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                rollback(&backups).await;
                return Err(FsError::Rollback {
                    path: raw.clone(),
                    source: error,
                });
            }
        };
        if let Some(parent) = path.parent()
            && let Err(error) = tokio::fs::create_dir_all(parent).await
        {
            rollback(&backups).await;
            return Err(FsError::Rollback {
                path: raw.clone(),
                source: error,
            });
        }
        if let Err(error) = tokio::fs::write(path, content).await {
            rollback(&backups).await;
            return Err(FsError::Rollback {
                path: raw.clone(),
                source: error,
            });
        }
        backups.push((path.clone(), backup));
    }

    Ok(json!({ "written": staged.len() }))
}

async fn rollback(backups: &[(PathBuf, Option<Vec<u8>>)]) {
    for (path, backup) in backups.iter().rev() {
        let _ = match backup {
            Some(bytes) => tokio::fs::write(path, bytes).await,
            None => tokio::fs::remove_file(path).await,
        };
    }
}

pub(crate) async fn list_dir(executor: &FsExecutor, input: &Value) -> Result<Value, FsError> {
    let base = executor.resolve(required_str(input, "path")?)?;
    let recursive = input
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let matcher = input
        .get("glob")
        .and_then(Value::as_str)
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| FsError::InvalidGlob(error.to_string()))
        })
        .transpose()?;
    let max_entries = executor.max_entries();

    let mut entries = Vec::new();
    let mut truncated = false;
    let mut queue = vec![(base.clone(), String::new())];
    while let Some((dir, prefix)) = queue.pop() {
        let mut reader = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = reader.next_entry().await? {
            let file_type = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };

            if file_type.is_dir() && recursive {
                queue.push((entry.path(), relative.clone()));
            }
            if let Some(matcher) = &matcher
                && !matcher.is_match(&relative)
            {
                continue;
            }
            if entries.len() >= max_entries {
                truncated = true;
                queue.clear();
                break;
            }
            entries.push(json!({
                "name": relative,
                "kind": if file_type.is_dir() { "dir" } else if file_type.is_symlink() { "symlink" } else { "file" },
            }));
        }
    }

    entries.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
    });
    let mut output = json!({ "entries": entries });
    if truncated {
        output["truncated"] = json!(true);
    }
    Ok(output)
}
