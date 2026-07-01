//! Runtime configuration file: adapter bindings and tool capabilities.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;
use tool_compiler_adapter_fs::FsExecutor;
use tool_compiler_adapter_http::{HttpExecutor, ReqwestHttpClient};
use tool_compiler_adapter_mcp::{
    McpExecutor, McpServerConfig, McpStdioClient, McpTransport, derive_capabilities,
};
use tool_compiler_adapter_shell::ShellExecutor;
use tool_compiler_runtime::{Runtime, ToolCapabilities, ToolRegistry};

use crate::CliError;
use crate::local::LocalExecutor;

/// Top-level runtime configuration file.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    mcp: Vec<McpBinding>,
    #[serde(default)]
    fs: Vec<FsBinding>,
    #[serde(default)]
    shell: Vec<ShellBinding>,
    #[serde(default)]
    http: Vec<HttpBinding>,
    #[serde(default)]
    capabilities: Vec<CapabilityBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpBinding {
    adapter: String,
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_clear: bool,
    #[serde(default)]
    inherit: Vec<String>,
    #[serde(default)]
    request_timeout_ms: Option<u64>,
    /// List the server's tools at startup and register adapter-scoped
    /// capabilities derived from their MCP annotations.
    #[serde(default)]
    hydrate: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FsBinding {
    adapter: String,
    root: PathBuf,
    #[serde(default)]
    max_read_bytes: Option<usize>,
    #[serde(default)]
    max_entries: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellBinding {
    adapter: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_clear: bool,
    #[serde(default)]
    inherit: Vec<String>,
    #[serde(default)]
    default_timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpBinding {
    adapter: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    default_headers: BTreeMap<String, String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityBinding {
    tool: String,
    /// Scope the capabilities to one adapter (wins over unscoped entries).
    #[serde(default)]
    adapter: Option<String>,
    #[serde(flatten)]
    capabilities: ToolCapabilities,
}

/// Builds a runtime from an optional config file. The `local` example
/// adapter and the `builtin` data adapter are always registered.
pub async fn configured_runtime(config_path: Option<PathBuf>) -> Result<Runtime, CliError> {
    let mut registry = ToolRegistry::new().with_adapter("local", LocalExecutor);
    // Defense in depth against recursive composition: a plan must never call
    // the compiler's own model-visible tools through a configured adapter.
    for tool in crate::serve::SERVED_TOOLS {
        registry.block_tool(tool);
    }

    let Some(path) = config_path else {
        return Ok(Runtime::from_registry(registry));
    };
    let content = crate::read_input(&path)?;
    let config: RuntimeConfig = serde_json::from_str(&content)
        .map_err(|error| CliError::config(&path, error.to_string()))?;

    for server in config.mcp {
        let client = McpStdioClient::new(McpServerConfig {
            name: server.name.clone(),
            transport: McpTransport::Stdio {
                command: server.command,
                args: server.args,
                env: server.env,
                env_clear: server.env_clear,
                inherit: server.inherit,
            },
            request_timeout_ms: server.request_timeout_ms,
        });
        if server.hydrate {
            let derived = derive_capabilities(&server.name, &client)
                .await
                .map_err(|error| {
                    CliError::config(&path, format!("hydrating '{}': {error}", server.adapter))
                })?;
            for capability in derived {
                let mut capabilities = ToolCapabilities::new().with_effects(capability.effects);
                if let Some(schema) = capability.input_schema {
                    capabilities = capabilities.with_input_schema(schema);
                }
                registry.register_adapter_tool_capabilities(
                    &server.adapter,
                    &capability.tool,
                    capabilities,
                );
            }
        }
        registry.register_adapter(server.adapter, McpExecutor::new(client));
    }

    for fs in config.fs {
        let mut executor = FsExecutor::new(fs.root);
        if let Some(max_bytes) = fs.max_read_bytes {
            executor = executor.with_max_read_bytes(max_bytes);
        }
        if let Some(max_entries) = fs.max_entries {
            executor = executor.with_max_entries(max_entries);
        }
        registry.register_adapter(fs.adapter, executor);
    }

    for shell in config.shell {
        let mut executor = ShellExecutor::new().with_env_clear(shell.env_clear);
        if let Some(cwd) = shell.cwd {
            executor = executor.with_cwd(cwd);
        }
        for (key, value) in shell.env {
            executor = executor.with_env(key, value);
        }
        for key in shell.inherit {
            executor = executor.with_inherited_var(key);
        }
        if let Some(timeout) = shell.default_timeout_ms {
            executor = executor.with_default_timeout_ms(timeout);
        }
        if let Some(max_bytes) = shell.max_output_bytes {
            executor = executor.with_max_output_bytes(max_bytes);
        }
        registry.register_adapter(shell.adapter, executor);
    }

    for http in config.http {
        let mut client = match http.timeout_ms {
            Some(timeout) => {
                ReqwestHttpClient::with_timeout(std::time::Duration::from_millis(timeout))
            }
            None => ReqwestHttpClient::new(),
        }
        .map_err(|error| CliError::config(&path, error.to_string()))?;
        if let Some(base_url) = http.base_url {
            client = client.with_base_url(base_url);
        }
        for (name, value) in http.default_headers {
            client = client.with_default_header(name, value);
        }
        registry.register_adapter(http.adapter, HttpExecutor::new(client));
    }

    for capability in config.capabilities {
        match capability.adapter {
            Some(adapter) => registry.register_adapter_tool_capabilities(
                adapter,
                capability.tool,
                capability.capabilities,
            ),
            None => registry.register_tool_capabilities(capability.tool, capability.capabilities),
        }
    }

    Ok(Runtime::from_registry(registry))
}

/// Parses `--param key=value` pairs; values parse as JSON when possible and
/// fall back to strings.
pub fn parse_params(raw: &[String]) -> Result<BTreeMap<String, Value>, CliError> {
    let mut params = BTreeMap::new();
    for pair in raw {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(CliError::Input(format!(
                "--param expects key=value, got '{pair}'"
            )));
        };
        let value = serde_json::from_str::<Value>(value)
            .unwrap_or_else(|_| Value::String(value.to_owned()));
        params.insert(key.to_owned(), value);
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn builds_a_runtime_from_every_binding_kind() {
        let config = serde_json::json!({
            "mcp": [{
                "adapter": "mcp.files",
                "name": "files",
                "command": "node",
                "args": ["-e", "process.exit(0)"],
                "env_clear": true,
                "inherit": ["PATH"],
                "request_timeout_ms": 5000
            }],
            "fs": [{ "adapter": "fs.repo", "root": ".", "max_read_bytes": 1024, "max_entries": 10 }],
            "shell": [{
                "adapter": "shell.local",
                "cwd": ".",
                "env": { "X": "1" },
                "env_clear": true,
                "inherit": ["PATH"],
                "default_timeout_ms": 1000,
                "max_output_bytes": 4096
            }],
            "http": [{
                "adapter": "http.api",
                "base_url": "https://example.test",
                "default_headers": { "authorization": "Bearer x" },
                "timeout_ms": 2000
            }],
            "capabilities": [
                { "tool": "read_file", "effects": { "pure": true } },
                { "tool": "read_file", "adapter": "fs.repo", "version": "scoped" }
            ]
        });
        let dir = std::env::temp_dir().join(format!(
            "tool-compiler-config-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runtime.json");
        std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();

        let runtime = configured_runtime(Some(path)).await.unwrap();
        let registry = runtime.registry();

        for adapter in [
            "local",
            "builtin",
            "mcp.files",
            "fs.repo",
            "shell.local",
            "http.api",
        ] {
            assert!(
                registry.executor_for(adapter).is_some(),
                "missing {adapter}"
            );
        }
        // Adapter-scoped capabilities win over the unscoped entry.
        assert_eq!(
            registry
                .capabilities_for("fs.repo", "read_file")
                .unwrap()
                .version
                .as_deref(),
            Some("scoped")
        );
        assert!(
            registry
                .capabilities_for("other", "read_file")
                .unwrap()
                .effects
                .is_some()
        );
        // The compiler's own tools are always blocked (recursion defense).
        assert!(registry.is_blocked("run_compiled_tool_graph"));
    }

    #[tokio::test]
    async fn hydrate_registers_capabilities_from_the_servers_tool_list() {
        let script = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0.1.0' } }
    }));
  } else if (req.method === 'tools/list') {
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: { tools: [{ name: 'lookup', inputSchema: { type: 'object' }, annotations: { readOnlyHint: true, idempotentHint: true } }] }
    }));
  }
});
"#;
        let config = serde_json::json!({
            "mcp": [{
                "adapter": "mcp.fake",
                "name": "fake",
                "command": "node",
                "args": ["-e", script],
                "hydrate": true
            }]
        });
        let dir = std::env::temp_dir().join(format!(
            "tool-compiler-hydrate-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runtime.json");
        std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();

        let runtime = configured_runtime(Some(path)).await.unwrap();

        let capabilities = runtime
            .registry()
            .capabilities_for("mcp.fake", "lookup")
            .expect("hydrated capabilities");
        assert!(
            capabilities
                .effects
                .as_ref()
                .unwrap()
                .reads
                .contains("mcp:fake")
        );
        assert!(capabilities.input_schema.is_some());
    }

    #[tokio::test]
    async fn rejects_unknown_config_fields() {
        let dir = std::env::temp_dir().join(format!(
            "tool-compiler-config-bad-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runtime.json");
        std::fs::write(&path, r#"{ "mpc": [] }"#).unwrap();

        let Err(error) = configured_runtime(Some(path)).await else {
            panic!("unknown config fields must be rejected");
        };

        assert!(error.to_string().contains("mpc"));
    }

    #[test]
    fn parses_params_as_json_with_string_fallback() {
        let params = parse_params(&[
            "count=3".to_owned(),
            "flag=true".to_owned(),
            "name=plain text".to_owned(),
        ])
        .unwrap();

        assert_eq!(params["count"], serde_json::json!(3));
        assert_eq!(params["flag"], serde_json::json!(true));
        assert_eq!(params["name"], serde_json::json!("plain text"));
    }

    #[test]
    fn rejects_malformed_params() {
        assert!(parse_params(&["broken".to_owned()]).is_err());
    }
}
