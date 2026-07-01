//! Stable intermediate representation for effect-aware agent tool plans.
//!
//! A [`Plan`] declares tools (with their [`Effects`]), graph nodes, and named
//! outputs. Plans are data: they arrive as JSON (often authored by an LLM or
//! an SDK builder), are validated by `tool-compiler-graph`, optimized by
//! `tool-compiler-optimizer`, and executed by `tool-compiler-runtime`.
//!
//! Unknown JSON fields are rejected everywhere (`deny_unknown_fields`): a
//! typo in an LLM-authored plan must fail loudly at parse time, not be
//! silently dropped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod canonical;
mod effects;
mod refs;

pub use canonical::canonical_json_string;
pub use effects::{Effects, RetryPolicy, resolve_resource_template};
pub use refs::{LITERAL_KEY, REF_KEY, RefParseError, ValueRef, collect_refs, validate_node_id};

/// Identifier of a graph node. Must not contain `.` (see [`validate_node_id`]).
pub type NodeId = String;
/// Name of a tool declared in a plan's `tools` map.
pub type ToolName = String;
/// Abstract resource named by effect declarations.
pub type Resource = String;

/// Plan IR version accepted by this crate.
pub const CURRENT_VERSION: &str = "0";

/// Input key that marks the fan-out item placeholder inside a `for_each`
/// node's input template: `{"$item": ""}` for the whole item, or
/// `{"$item": "field.path"}` for a path inside it.
pub const ITEM_KEY: &str = "$item";

/// An executable tool plan: tools, nodes, and requested outputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Plan IR version; must equal [`CURRENT_VERSION`].
    pub version: String,
    /// Optional human label used in traces, transcripts, and reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional human description of what the plan does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tools referenced by nodes, keyed by tool name.
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    /// Graph nodes.
    #[serde(default)]
    pub nodes: Vec<Node>,
    /// Named outputs resolved from node outputs after execution.
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl Plan {
    /// Creates an empty plan at [`CURRENT_VERSION`].
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            name: None,
            description: None,
            tools: BTreeMap::new(),
            nodes: Vec::new(),
            outputs: BTreeMap::new(),
        }
    }

    /// Sets the human-readable plan name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

/// Declaration of a tool: which adapter executes it and what it promises.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Adapter name this tool dispatches through.
    pub adapter: String,
    /// Declared effects; absent or vacuous effects block unsafe optimization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
    /// Scheduler limits (batch size, concurrency ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ToolLimits>,
    /// Cost metadata for planning and benchmark estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ToolCost>,
    /// Tool behavior version; changing it invalidates cached outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ToolSpec {
    /// Creates a spec for `adapter` with no metadata.
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
            effects: None,
            limits: None,
            cost: None,
            version: None,
        }
    }

    /// Attaches effect declarations.
    pub fn with_effects(mut self, effects: Effects) -> Self {
        self.effects = Some(effects);
        self
    }

    /// Attaches scheduler limits.
    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Attaches cost metadata.
    pub fn with_cost(mut self, cost: ToolCost) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Attaches a tool behavior version (part of the cache key).
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }
}

/// Scheduler constraints for one tool.
///
/// Limits bound how much execution happens at once; effects decide whether
/// the execution is legal at all. `max_concurrency` counts concurrent
/// dispatches of the tool (a batch dispatch counts as one), and `batch_size`
/// caps how many nodes join one batch dispatch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolLimits {
    /// Maximum concurrent dispatches of this tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    /// Maximum nodes joined into one batch dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
}

/// Cost metadata used for planning and benchmark estimates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCost {
    /// Fixed latency per dispatch, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_ms: Option<u64>,
    /// Additional latency per call, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_call_ms: Option<u64>,
    /// Estimated tokens the call's result costs in a transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
}

/// One node of the plan graph: a tool call with an input and dependencies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Unique node id (see [`validate_node_id`]).
    pub id: NodeId,
    /// Name of the tool to call, declared in [`Plan::tools`].
    pub tool: ToolName,
    /// JSON input; may contain `{"$ref": ...}` references and
    /// `{"$literal": ...}` escapes.
    #[serde(default)]
    pub input: Value,
    /// Explicit dependencies in addition to those derived from references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<NodeId>,
    /// When present, the node expands at runtime into one call per element
    /// of the referenced array. `{"$item": <path>}` markers inside `input`
    /// are substituted per element, and the node's output becomes the array
    /// of per-element outputs, in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_each: Option<ValueRef>,
    /// When present, the node only runs if the condition holds; otherwise it
    /// is skipped and nodes depending on it are skipped transitively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
}

impl Node {
    /// Creates a node calling `tool` with an empty object input.
    pub fn new(id: impl Into<NodeId>, tool: impl Into<ToolName>) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            input: Value::Object(Default::default()),
            depends_on: Vec::new(),
            for_each: None,
            when: None,
        }
    }

    /// Sets the node input.
    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    /// Sets the fan-out source for a dynamic node.
    pub fn with_for_each(mut self, items: ValueRef) -> Self {
        self.for_each = Some(items);
        self
    }

    /// Sets the run condition.
    pub fn with_when(mut self, when: When) -> Self {
        self.when = Some(when);
        self
    }
}

/// A runtime condition gating a node.
///
/// The referenced value is resolved after the node's dependencies finish.
/// With `equals`, the node runs iff the resolved value equals it. Without
/// `equals`, the node runs iff the resolved value is truthy (not `null` and
/// not `false`). `not` inverts the verdict.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Reference resolved to decide whether the node runs.
    #[serde(rename = "ref")]
    pub target: ValueRef,
    /// Expected value; when absent, a truthiness check is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    /// Inverts the verdict.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not: bool,
}

impl When {
    /// Condition: run when `target` resolves truthy.
    pub fn truthy(target: ValueRef) -> Self {
        Self {
            target,
            equals: None,
            not: false,
        }
    }

    /// Condition: run when `target` resolves equal to `value`.
    pub fn equals(target: ValueRef, value: Value) -> Self {
        Self {
            target,
            equals: Some(value),
            not: false,
        }
    }

    /// Inverts the condition.
    pub fn negated(mut self) -> Self {
        self.not = !self.not;
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_tool_limits_when_present() {
        let spec = ToolSpec::new("test").with_limits(ToolLimits {
            max_concurrency: Some(2),
            batch_size: Some(10),
        });
        let value = serde_json::to_value(spec).unwrap();

        assert_eq!(value["limits"]["max_concurrency"], 2);
        assert_eq!(value["limits"]["batch_size"], 10);
    }

    #[test]
    fn serializes_tool_cost_when_present() {
        let spec = ToolSpec::new("test").with_cost(ToolCost {
            fixed_ms: Some(50),
            per_call_ms: Some(5),
            tokens: Some(120),
        });
        let value = serde_json::to_value(spec).unwrap();

        assert_eq!(value["cost"]["fixed_ms"], 50);
        assert_eq!(value["cost"]["per_call_ms"], 5);
        assert_eq!(value["cost"]["tokens"], 120);
    }

    #[test]
    fn plan_metadata_round_trips() {
        let plan = Plan::new().with_name("lookup docs");
        let value = serde_json::to_value(&plan).unwrap();

        assert_eq!(value["name"], "lookup docs");
        assert_eq!(serde_json::from_value::<Plan>(value).unwrap(), plan);
    }

    #[test]
    fn rejects_unknown_plan_fields() {
        let error = serde_json::from_value::<Plan>(json!({
            "version": "0",
            "nodez": []
        }))
        .unwrap_err();

        assert!(error.to_string().contains("nodez"));
    }

    #[test]
    fn rejects_unknown_node_fields() {
        let error = serde_json::from_value::<Node>(json!({
            "id": "a",
            "tool": "echo",
            "depend_on": ["b"]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("depend_on"));
    }

    #[test]
    fn node_for_each_and_when_round_trip() {
        let node = Node::new("send", "notify")
            .with_input(json!({ "target": { "$item": "email" } }))
            .with_for_each(ValueRef::new("users", ["list"]))
            .with_when(When::equals(ValueRef::output("gate"), json!(true)));

        let value = serde_json::to_value(&node).unwrap();

        assert_eq!(value["for_each"], "users.output.list");
        assert_eq!(value["when"]["ref"], "gate.output");
        assert_eq!(value["when"]["equals"], true);
        assert_eq!(serde_json::from_value::<Node>(value).unwrap(), node);
    }

    #[test]
    fn tool_version_round_trips() {
        let spec = ToolSpec::new("test").with_version("v2");
        let value = serde_json::to_value(&spec).unwrap();

        assert_eq!(value["version"], "v2");
    }
}
