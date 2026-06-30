use tool_compiler_ir::{Effects, ToolSpec};

pub const ADAPTER: &str = "mcp";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String> },
    WebSocket { url: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpToolBinding {
    pub server: McpServerConfig,
    pub tool_name: String,
    pub spec: ToolSpec,
}

impl McpToolBinding {
    pub fn new(server: McpServerConfig, tool_name: impl Into<String>, spec: ToolSpec) -> Self {
        Self {
            server,
            tool_name: tool_name.into(),
            spec,
        }
    }

    pub fn tool_key(&self) -> String {
        format!("{}:{}", self.server.name, self.tool_name)
    }
}

pub fn pure_tool() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::pure())
}

pub fn read_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

pub fn effectful_tool(
    reads: impl IntoIterator<Item = impl Into<String>>,
    writes: impl IntoIterator<Item = impl Into<String>>,
) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        reads: reads.into_iter().map(Into::into).collect(),
        writes: writes.into_iter().map(Into::into).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_tool_is_cacheable() {
        let effects = pure_tool().effects.unwrap();

        assert!(effects.pure);
        assert!(effects.cacheable);
    }

    #[test]
    fn effectful_tool_declares_reads_and_writes() {
        let effects = effectful_tool(["fs:read"], ["fs:write"]).effects.unwrap();

        assert!(effects.reads.contains("fs:read"));
        assert!(effects.writes.contains("fs:write"));
        assert!(!effects.cacheable);
    }

    #[test]
    fn read_tool_declares_read_only_effects() {
        let effects = read_tool(["mcp:resource"]).effects.unwrap();

        assert!(effects.reads.contains("mcp:resource"));
        assert!(effects.writes.is_empty());
    }

    #[test]
    fn binding_builds_stable_tool_keys() {
        let binding = McpToolBinding::new(
            McpServerConfig {
                name: "filesystem".into(),
                transport: McpTransport::Stdio {
                    command: "node".into(),
                    args: vec!["server.js".into()],
                },
            },
            "read_file",
            read_tool(["fs:workspace"]),
        );

        assert_eq!(binding.tool_key(), "filesystem:read_file");
        assert_eq!(binding.spec.adapter, ADAPTER);
    }
}
