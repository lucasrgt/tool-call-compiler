//! Filesystem adapter, contained to a root directory.
//!
//! Tools: `read_file`, `write_file`, `write_files` (transactional multi-file
//! write with rollback), and `list_dir` (typed entries, optional recursion
//! and glob filtering). Every path is validated syntactically (no absolute
//! paths, no `..`) **and** canonically: the deepest existing ancestor of the
//! target is canonicalized and must stay inside the canonicalized root, so
//! symlinks cannot escape the sandbox.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tokio::task::JoinSet;
use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{Effects, ToolSpec};

mod ops;

use ops::{list_dir, read_file, write_file, write_files};

/// Conventional adapter name for filesystem executors.
pub const ADAPTER: &str = "fs";

/// Default cap on bytes returned by `read_file`.
pub const DEFAULT_MAX_READ_BYTES: usize = 2 * 1024 * 1024;
/// Default cap on entries returned by `list_dir`.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;
/// Concurrent reads used by the native batch implementation.
const BATCH_CONCURRENCY: usize = 16;

/// Filesystem executor rooted at a directory.
#[derive(Clone, Debug)]
pub struct FsExecutor {
    root: PathBuf,
    max_read_bytes: usize,
    max_entries: usize,
}

impl FsExecutor {
    /// Creates an executor contained to `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }

    /// Overrides the default `read_file` byte cap.
    pub fn with_max_read_bytes(mut self, max_bytes: usize) -> Self {
        self.max_read_bytes = max_bytes.max(1);
        self
    }

    /// Overrides the default `list_dir` entry cap.
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries.max(1);
        self
    }

    pub(crate) fn max_read_bytes(&self, input: &Value) -> usize {
        input
            .get("max_bytes")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(self.max_read_bytes)
            .max(1)
    }

    pub(crate) fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Resolves a relative path inside the root, rejecting absolute paths,
    /// parent traversal, and symlink escapes.
    pub(crate) fn resolve(&self, raw: &str) -> Result<PathBuf, FsError> {
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

        let joined = self.root.join(path);
        self.check_containment(raw, &joined)?;
        Ok(joined)
    }

    /// Canonicalizes the deepest existing ancestor of `joined` and requires
    /// it to remain inside the canonicalized root. This is what blocks
    /// symlinks that point outside the sandbox.
    fn check_containment(&self, raw: &str, joined: &Path) -> Result<(), FsError> {
        let Ok(canonical_root) = std::fs::canonicalize(&self.root) else {
            // Root does not exist yet: nothing inside it can escape.
            return Ok(());
        };

        let mut probe = joined.to_path_buf();
        while !probe.exists() {
            match probe.parent() {
                Some(parent) => probe = parent.to_path_buf(),
                None => return Ok(()),
            }
        }
        let canonical = std::fs::canonicalize(&probe)?;
        if canonical.starts_with(&canonical_root) {
            Ok(())
        } else {
            Err(FsError::EscapesRoot(raw.into()))
        }
    }
}

#[async_trait]
impl ToolExecutor for FsExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        execute(self, tool, input).await.map_err(tool_error)
    }

    /// Native batch: reads run concurrently (bounded), preserving the
    /// node-to-output mapping.
    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        let mut outputs = Vec::with_capacity(inputs.len());
        let mut window: JoinSet<Result<BatchOutput, ToolExecutionError>> = JoinSet::new();
        let mut queue = inputs.into_iter();

        loop {
            while window.len() < BATCH_CONCURRENCY {
                let Some(input) = queue.next() else { break };
                let executor = self.clone();
                let tool = tool.to_owned();
                window.spawn(async move {
                    Ok(BatchOutput {
                        output: executor.call(&tool, input.input).await?,
                        node: input.node,
                    })
                });
            }
            match window.join_next().await {
                Some(joined) => {
                    let output = joined
                        .map_err(|error| ToolExecutionError::new(error.to_string()))??;
                    outputs.push(output);
                }
                None => break,
            }
        }

        Ok(outputs)
    }
}

/// ToolSpec for `read_file`: batchable, cacheable, read-only. Resources may
/// use templates such as `file:{path}` for per-node granularity.
pub fn read_file_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        batchable: true,
        ..Effects::read_only(resources)
    })
}

/// ToolSpec for `write_file`: idempotent full-content overwrite.
pub fn write_file_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: true,
        ..Effects::default()
    })
}

/// ToolSpec for `write_files`: transactional multi-file write.
pub fn write_files_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    write_file_tool(resources)
}

/// ToolSpec for `list_dir`: read-only directory listing.
pub fn list_dir_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

async fn execute(executor: &FsExecutor, tool: &str, input: Value) -> Result<Value, FsError> {
    match tool {
        "read_file" => read_file(executor, &input).await,
        "write_file" => write_file(executor, &input).await,
        "write_files" => write_files(executor, &input).await,
        "list_dir" => list_dir(executor, &input).await,
        other => Err(FsError::UnknownTool(other.into())),
    }
}

pub(crate) fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, FsError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| FsError::MissingField(key.into()))
}

fn tool_error(error: FsError) -> ToolExecutionError {
    let code = match &error {
        FsError::MissingField(_) | FsError::InvalidGlob(_) => "invalid_input",
        FsError::EscapesRoot(_) => "escapes_root",
        FsError::UnknownTool(_) => "unknown_tool",
        FsError::Rollback { .. } => "write_rollback",
        FsError::Io(_) => "io",
    };
    ToolExecutionError::new(error.to_string()).with_code(code)
}

/// Filesystem adapter errors.
#[derive(Debug, Error)]
pub enum FsError {
    /// A required string field is missing from the input.
    #[error("missing string field '{0}'")]
    MissingField(String),
    /// The path leaves the sandbox root (absolute, `..`, or via symlink).
    #[error("path escapes filesystem root: {0}")]
    EscapesRoot(String),
    /// The glob pattern is invalid.
    #[error("invalid glob pattern: {0}")]
    InvalidGlob(String),
    /// The tool name is not one of the filesystem tools.
    #[error("unknown fs tool '{0}'")]
    UnknownTool(String),
    /// A transactional write failed and previous files were restored.
    #[error("write_files failed on '{path}' and was rolled back: {source}")]
    Rollback {
        /// File where the failure happened.
        path: String,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// Underlying I/O error.
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
        assert_eq!(list["entries"][0]["name"], "a.txt");
        assert_eq!(list["entries"][0]["kind"], "file");
    }

    #[tokio::test]
    async fn rejects_paths_that_escape_root() {
        let error = FsExecutor::new(temp_root())
            .call("read_file", json!({ "path": "../secret.txt" }))
            .await
            .unwrap_err();

        assert_eq!(error.code.as_deref(), Some("escapes_root"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlinks_that_escape_root() {
        let root = temp_root();
        std::fs::create_dir_all(&root).unwrap();
        let outside = temp_root();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let error = FsExecutor::new(&root)
            .call("read_file", json!({ "path": "link/secret.txt" }))
            .await
            .unwrap_err();

        assert_eq!(error.code.as_deref(), Some("escapes_root"));
    }

    #[tokio::test]
    async fn truncates_reads_beyond_the_byte_cap() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);
        executor
            .call(
                "write_file",
                json!({ "path": "big.txt", "content": "x".repeat(100) }),
            )
            .await
            .unwrap();

        let read = executor
            .call("read_file", json!({ "path": "big.txt", "max_bytes": 10 }))
            .await
            .unwrap();

        assert_eq!(read["truncated"], true);
        assert_eq!(read["bytes"], 100);
        assert_eq!(read["content"].as_str().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn lists_recursively_with_glob_filters() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);
        for path in ["src/a.rs", "src/deep/b.rs", "src/readme.md"] {
            executor
                .call("write_file", json!({ "path": path, "content": "" }))
                .await
                .unwrap();
        }

        let list = executor
            .call(
                "list_dir",
                json!({ "path": "src", "recursive": true, "glob": "**/*.rs" }),
            )
            .await
            .unwrap();

        let names: Vec<&str> = list["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a.rs", "deep/b.rs"]);
    }

    #[tokio::test]
    async fn write_files_rolls_back_on_failure() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);
        executor
            .call("write_file", json!({ "path": "keep.txt", "content": "old" }))
            .await
            .unwrap();

        // Second entry escapes the root: resolution fails before anything is
        // written, and the first file keeps its previous content.
        let error = executor
            .call(
                "write_files",
                json!({ "files": [
                    { "path": "keep.txt", "content": "new" },
                    { "path": "../escape.txt", "content": "x" }
                ]}),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("escapes_root"));

        let read = executor
            .call("read_file", json!({ "path": "keep.txt" }))
            .await
            .unwrap();
        assert_eq!(read["content"], "old");
    }

    #[tokio::test]
    async fn write_files_writes_all_files() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);

        let output = executor
            .call(
                "write_files",
                json!({ "files": [
                    { "path": "a.txt", "content": "1" },
                    { "path": "dir/b.txt", "content": "2" }
                ]}),
            )
            .await
            .unwrap();

        assert_eq!(output["written"], 2);
        let read = executor
            .call("read_file", json!({ "path": "dir/b.txt" }))
            .await
            .unwrap();
        assert_eq!(read["content"], "2");
    }

    #[tokio::test]
    async fn native_batch_preserves_node_mapping() {
        let root = temp_root();
        let executor = FsExecutor::new(&root);
        for (path, content) in [("a.txt", "A"), ("b.txt", "B")] {
            executor
                .call("write_file", json!({ "path": path, "content": content }))
                .await
                .unwrap();
        }

        let outputs = executor
            .call_batch(
                "read_file",
                vec![
                    BatchInput {
                        node: "a".into(),
                        input: json!({ "path": "a.txt" }),
                    },
                    BatchInput {
                        node: "b".into(),
                        input: json!({ "path": "b.txt" }),
                    },
                ],
            )
            .await
            .unwrap();

        let by_node: std::collections::BTreeMap<&str, &Value> = outputs
            .iter()
            .map(|output| (output.node.as_str(), &output.output))
            .collect();
        assert_eq!(by_node["a"]["content"], "A");
        assert_eq!(by_node["b"]["content"], "B");
    }

    #[test]
    fn declares_tool_effects() {
        let read = read_file_tool(["file:{path}"]);
        let write = write_file_tool(["file:{path}"]);

        assert!(read.effects.unwrap().batchable);
        assert!(write.effects.unwrap().writes.contains("file:{path}"));
    }
}
