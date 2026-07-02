//! Adapter and capability registry.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_compiler_adapter_api::ToolExecutor;
use tool_compiler_ir::{Effects, Plan, ToolCost, ToolLimits, ToolSpec};

use crate::builtin::{BuiltinExecutor, register_builtin_capabilities};

/// Adapter name automatically registered on every runtime for the built-in
/// pure data tools (`collect`, `pick`, `merge`, `template`).
pub const BUILTIN_ADAPTER: &str = "builtin";

/// Registry metadata for one tool: effects, limits, cost, version, schemas.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCapabilities {
    /// Effects hydrated into plans that omit them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
    /// Limits hydrated into plans that omit them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ToolLimits>,
    /// Cost hydrated into plans that omit them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ToolCost>,
    /// Tool behavior version hydrated into plans that omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// JSON Schema each resolved input is validated against before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// JSON Schema of the tool output (documentation; not enforced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl ToolCapabilities {
    /// Creates empty capabilities.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches effects.
    pub fn with_effects(mut self, effects: Effects) -> Self {
        self.effects = Some(effects);
        self
    }

    /// Attaches limits.
    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Attaches cost metadata.
    pub fn with_cost(mut self, cost: ToolCost) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Attaches a tool behavior version.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Attaches an input schema enforced before dispatch.
    pub fn with_input_schema(mut self, schema: Value) -> Self {
        self.input_schema = Some(schema);
        self
    }
}

/// Maps adapter names to executors and tools to capabilities.
///
/// Capabilities can be registered globally by tool name or scoped to one
/// adapter (`register_adapter_tool_capabilities`); the adapter-scoped entry
/// wins, so two adapters exposing a tool with the same name no longer share
/// metadata by accident.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    adapters: BTreeMap<String, Arc<dyn ToolExecutor>>,
    capabilities: BTreeMap<String, ToolCapabilities>,
    /// Adapter-scoped capabilities, nested by adapter then tool so lookups
    /// borrow both names instead of allocating a tuple key.
    adapter_capabilities: BTreeMap<String, BTreeMap<String, ToolCapabilities>>,
    blocked_tools: BTreeSet<String>,
}

impl ToolRegistry {
    /// Creates a registry with the built-in pure data adapter registered.
    pub fn new() -> Self {
        let mut registry = Self::default();
        registry.register_adapter(BUILTIN_ADAPTER, BuiltinExecutor);
        register_builtin_capabilities(&mut registry);
        registry
    }

    /// Registers an adapter, replacing any previous executor of that name.
    pub fn with_adapter(
        mut self,
        adapter: impl Into<String>,
        executor: impl ToolExecutor + 'static,
    ) -> Self {
        self.register_adapter(adapter, executor);
        self
    }

    /// Registers an adapter in place.
    pub fn register_adapter(
        &mut self,
        adapter: impl Into<String>,
        executor: impl ToolExecutor + 'static,
    ) {
        self.adapters.insert(adapter.into(), Arc::new(executor));
    }

    /// Registers capabilities for a tool name across all adapters.
    pub fn with_tool_capabilities(
        mut self,
        tool: impl Into<String>,
        capabilities: ToolCapabilities,
    ) -> Self {
        self.register_tool_capabilities(tool, capabilities);
        self
    }

    /// Registers capabilities for a tool name across all adapters, in place.
    pub fn register_tool_capabilities(
        &mut self,
        tool: impl Into<String>,
        capabilities: ToolCapabilities,
    ) {
        self.capabilities.insert(tool.into(), capabilities);
    }

    /// Registers capabilities scoped to `(adapter, tool)`; wins over the
    /// unscoped entry.
    pub fn register_adapter_tool_capabilities(
        &mut self,
        adapter: impl Into<String>,
        tool: impl Into<String>,
        capabilities: ToolCapabilities,
    ) {
        self.adapter_capabilities
            .entry(adapter.into())
            .or_default()
            .insert(tool.into(), capabilities);
    }

    /// Blocks a tool name from executing (recursion / policy guard). Nodes
    /// calling a blocked tool fail with code `blocked_tool`.
    pub fn block_tool(&mut self, tool: impl Into<String>) {
        self.blocked_tools.insert(tool.into());
    }

    /// Blocked variant of [`Self::block_tool`] for builder chains.
    pub fn with_blocked_tool(mut self, tool: impl Into<String>) -> Self {
        self.block_tool(tool);
        self
    }

    /// Whether `tool` is blocked.
    pub fn is_blocked(&self, tool: &str) -> bool {
        self.blocked_tools.contains(tool)
    }

    /// Capabilities for `tool` under `adapter`, adapter-scoped entry first.
    pub fn capabilities_for(&self, adapter: &str, tool: &str) -> Option<&ToolCapabilities> {
        self.adapter_capabilities
            .get(adapter)
            .and_then(|tools| tools.get(tool))
            .or_else(|| self.capabilities.get(tool))
    }

    /// Unscoped capabilities for `tool`.
    pub fn capabilities(&self, tool: &str) -> Option<&ToolCapabilities> {
        self.capabilities.get(tool)
    }

    /// Fills missing effects/limits/cost/version on plan tools from the
    /// registered capabilities.
    pub(crate) fn apply_capabilities(&self, plan: &mut Plan) {
        for (tool, spec) in &mut plan.tools {
            if let Some(capabilities) = self.capabilities_for(&spec.adapter, tool) {
                merge_capabilities(spec, capabilities);
            }
        }
    }

    pub(crate) fn executor(&self, adapter: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.adapters.get(adapter).cloned()
    }

    /// Returns the executor registered for `adapter`, for direct use (for
    /// example, running the conformance suite against it).
    pub fn executor_for(&self, adapter: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.executor(adapter)
    }
}

fn merge_capabilities(spec: &mut ToolSpec, capabilities: &ToolCapabilities) {
    if spec.effects.is_none() {
        spec.effects = capabilities.effects.clone();
    }
    if spec.limits.is_none() {
        spec.limits = capabilities.limits.clone();
    }
    if spec.cost.is_none() {
        spec.cost = capabilities.cost.clone();
    }
    if spec.version.is_none() {
        spec.version = capabilities.version.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_scoped_capabilities_win_over_global() {
        let mut registry = ToolRegistry::new();
        registry.register_tool_capabilities(
            "read_file",
            ToolCapabilities::new().with_version("global"),
        );
        registry.register_adapter_tool_capabilities(
            "fs",
            "read_file",
            ToolCapabilities::new().with_version("scoped"),
        );

        assert_eq!(
            registry
                .capabilities_for("fs", "read_file")
                .unwrap()
                .version
                .as_deref(),
            Some("scoped")
        );
        assert_eq!(
            registry
                .capabilities_for("mcp", "read_file")
                .unwrap()
                .version
                .as_deref(),
            Some("global")
        );
    }

    #[test]
    fn blocked_tools_are_reported() {
        let registry = ToolRegistry::new().with_blocked_tool("run_compiled_tool_graph");

        assert!(registry.is_blocked("run_compiled_tool_graph"));
        assert!(!registry.is_blocked("echo"));
    }

    #[test]
    fn capability_builders_layer_metadata() {
        use tool_compiler_ir::{Effects, ToolCost, ToolLimits};

        let capabilities = ToolCapabilities::new()
            .with_effects(Effects::pure())
            .with_limits(ToolLimits {
                max_concurrency: Some(2),
                batch_size: Some(4),
            })
            .with_cost(ToolCost {
                fixed_ms: Some(1),
                per_call_ms: Some(2),
                tokens: Some(3),
            })
            .with_version("v9")
            .with_input_schema(serde_json::json!({ "type": "object" }));

        let registry = ToolRegistry::new().with_tool_capabilities("tool", capabilities.clone());

        let stored = registry.capabilities("tool").unwrap();
        assert_eq!(stored.version.as_deref(), Some("v9"));
        assert_eq!(stored.limits.as_ref().unwrap().batch_size, Some(4));
        assert_eq!(stored.cost.as_ref().unwrap().tokens, Some(3));
        assert!(stored.input_schema.is_some());
    }

    #[test]
    fn hydration_fills_only_missing_fields() {
        use tool_compiler_ir::{Effects, Node, Plan, ToolSpec};

        let registry = ToolRegistry::new().with_tool_capabilities(
            "echo",
            ToolCapabilities::new()
                .with_effects(Effects::pure())
                .with_version("from-registry"),
        );
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_version("from-plan"),
        );
        plan.nodes.push(Node::new("a", "echo"));

        registry.apply_capabilities(&mut plan);

        let spec = &plan.tools["echo"];
        assert!(spec.effects.as_ref().unwrap().pure);
        // Plan-declared metadata wins over registry hydration.
        assert_eq!(spec.version.as_deref(), Some("from-plan"));
    }
}
