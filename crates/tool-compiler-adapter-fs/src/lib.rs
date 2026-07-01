use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;
use tool_compiler_ir::{Effects, ToolSpec};
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};

pub const ADAPTER: &str = "fs";

#[derive(Clone, Debug)]
pub struct FsExecutor {
    root: PathBuf,
}

impl FsExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, raw: &str) -> Result<PathBuf, FsError> {
        let path = Path::new(raw);
        if path.is_absolute() {
            return Err(FsError::EscapesRoot(raw.into()));
        }

        for component in path.components() {
            if matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return Err(FsError::EscapesRoot(raw.into()));
            }
        }

        Ok(self.root.join(path))
    }
}

#[async_trait]
impl ToolExecutor for FsExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        execute(self, tool, input).await.map_err(tool_error)
    }
}

pub fn read_file_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        batchable: true,
        ..Effects::read_only(resources)
    })
}

pub fn write_file_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: true,
        ..Effects::default()
    })
}

async fn execute(executor: &FsExecutor, tool: &str, input: Value) -> Result<Value, FsError> {
    match tool {
        "read_file" => {
            let path = executor.resolve(required_str(&input, "path")?)?;
            let content = tokio::fs::read_to_string(path).await?;
            Ok(json!({ "content": content }))
        }
        "write_file" => {
            let path = executor.resolve(required_str(&input, "path")?)?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, required_str(&input, "content")?).await?;
            Ok(json!({ "path": input["path"], "written": true }))
        }
        "list_dir" => {
            let path = executor.resolve(required_str(&input, "path")?)?;
            let mut entries = tokio::fs::read_dir(path).await?;
            let mut names = Vec::new();
            while let Some(entry) = entries.next_entry().await? {
                names.push(entry.file_name().to_string_lossy().to_string());
            }
            names.sort();
            Ok(json!({ "entries": names }))
        }
        other => Err(FsError::UnknownTool(other.into())),
    }
}

fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, FsError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| FsError::MissingField(key.into()))
}

fn tool_error(error: FsError) -> ToolExecutionError {
    ToolExecutionError::new(error.to_string())
}

#[derive(Debug, Error)]
pub enum FsError {
    #[error("missing string field '{0}'")]
    MissingField(String),
    #[error("path escapes filesystem root: {0}")]
    EscapesRoot(String),
    #[error("unknown fs tool '{0}'")]
    UnknownTool(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    fn temp_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tool-compiler-fs-{id}"))
    }

    #[tokio::test]
    async fn writes_reads_and_lists_files() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);

        executor
            .call(
                "write_file",
                json!({ "path": "notes/a.txt", "content": "hello" }),
            )
            .await
            .unwrap();
        let read = executor
            .call("read_file", json!({ "path": "notes/a.txt" }))
            .await
            .unwrap();
        let list = executor
            .call("list_dir", json!({ "path": "notes" }))
            .await
            .unwrap();

        assert_eq!(read["content"], "hello");
        assert_eq!(list["entries"], json!(["a.txt"]));
    }

    #[tokio::test]
    async fn rejects_paths_that_escape_root() {
        let error = FsExecutor::new(temp_root())
            .call("read_file", json!({ "path": "../secret.txt" }))
            .await
            .unwrap_err();

        assert!(error.message.contains("escapes"));
    }

    #[test]
    fn declares_tool_effects() {
        let read = read_file_tool(["file:repo"]);
        let write = write_file_tool(["file:repo"]);

        assert!(read.effects.unwrap().batchable);
        assert!(write.effects.unwrap().writes.contains("file:repo"));
    }
}
