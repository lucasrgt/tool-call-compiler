//! Intent-to-plan compiler and recipe expansion for agent tool calls.
//!
//! Two higher-level inputs lower into the executable [`Plan`] IR:
//!
//! - **Intent plans** ([`IntentPlan`]): agent-authored steps whose
//!   dependencies are derived from `$ref`s plus explicit `after` ordering.
//! - **Recipes** ([`RecipePlan`]): compact declarative shapes (`fan_out`,
//!   `map_reduce`, `pipeline`) that expand into many nodes, optionally
//!   parameterized with `{"$param": "name"}` placeholders.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tool_compiler_ir::{
    CURRENT_VERSION, Node, NodeId, Plan, ToolName, ToolSpec, ValueRef, collect_refs,
    validate_node_id,
};

mod mining;
mod recipes;

pub use mining::{
    DEFAULT_MIN_OCCURRENCES, ObservedCall, Suggestion, SuggestionKind, suggest_recipes,
};
pub use recipes::{
    FanOutRecipe, MapReduceRecipe, PipelineRecipe, PipelineStep, Recipe, RecipePlan,
    compile_recipe, compile_recipe_with_params,
};

/// A higher-level agent-authored description of desired steps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentPlan {
    /// Intent IR version; must equal [`CURRENT_VERSION`].
    pub version: String,
    /// Optional human label carried into the compiled plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tools referenced by steps.
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    /// Ordered steps; dependencies derive from refs plus `after`.
    #[serde(default)]
    pub steps: Vec<IntentStep>,
    /// Named outputs of the compiled plan.
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl IntentPlan {
    /// Creates an empty intent at [`CURRENT_VERSION`].
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            name: None,
            tools: BTreeMap::new(),
            steps: Vec::new(),
            outputs: BTreeMap::new(),
        }
    }

    /// Sets the human-readable name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl Default for IntentPlan {
    fn default() -> Self {
        Self::new()
    }
}

/// One intent step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentStep {
    /// Step id (becomes the node id).
    pub id: NodeId,
    /// Tool the step calls.
    pub tool: ToolName,
    /// JSON input; may contain `$ref`s.
    #[serde(default)]
    pub input: Value,
    /// Explicit ordering dependencies in addition to derived refs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<NodeId>,
}

impl IntentStep {
    /// Creates a step calling `tool` with an empty object input.
    pub fn new(id: impl Into<NodeId>, tool: impl Into<ToolName>) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            input: Value::Object(Default::default()),
            after: Vec::new(),
        }
    }

    /// Sets the step input.
    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    /// Adds explicit ordering dependencies.
    pub fn after(mut self, dependencies: impl IntoIterator<Item = impl Into<NodeId>>) -> Self {
        self.after = dependencies.into_iter().map(Into::into).collect();
        self
    }
}

/// Errors produced while lowering intents and recipes.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlannerError {
    /// Unsupported IR version.
    #[error("unsupported intent version '{0}'")]
    UnsupportedVersion(String),
    /// Two steps share an id.
    #[error("duplicate step id '{0}'")]
    DuplicateStep(NodeId),
    /// A step id fails node-id validation.
    #[error("step '{step}' has an invalid id: {message}")]
    InvalidStepId {
        /// Offending step id.
        step: NodeId,
        /// Validation failure detail.
        message: String,
    },
    /// A step names a tool the intent does not declare.
    #[error("step '{step}' uses unknown tool '{tool}'")]
    UnknownTool {
        /// Offending step id.
        step: NodeId,
        /// Missing tool name.
        tool: ToolName,
    },
    /// A step references itself.
    #[error("step '{0}' references itself")]
    SelfReference(NodeId),
    /// A step input carries a malformed reference.
    #[error("invalid reference in step '{step}': {message}")]
    InvalidRef {
        /// Offending step id.
        step: NodeId,
        /// Parse failure detail.
        message: String,
    },
    /// A fan-out recipe has neither items nor an items_ref.
    #[error("fan_out recipe needs a non-empty 'items' array or an 'items_ref'")]
    EmptyFanOut,
    /// A pipeline recipe has no steps.
    #[error("pipeline recipe needs at least one step")]
    EmptyPipeline,
    /// A `$param` placeholder names an undeclared parameter.
    #[error("recipe references undeclared parameter '{0}'")]
    UnknownParam(String),
    /// A required parameter (declared with a null default) was not supplied.
    #[error("recipe parameter '{0}' is required but was not supplied")]
    MissingParam(String),
}

/// Compiles an intent into an executable plan.
pub fn compile_intent(intent: IntentPlan) -> Result<Plan, PlannerError> {
    if intent.version != CURRENT_VERSION {
        return Err(PlannerError::UnsupportedVersion(intent.version));
    }

    let mut seen = BTreeSet::new();
    for step in &intent.steps {
        validate_node_id(&step.id).map_err(|error| PlannerError::InvalidStepId {
            step: step.id.clone(),
            message: error.to_string(),
        })?;
        if !seen.insert(step.id.clone()) {
            return Err(PlannerError::DuplicateStep(step.id.clone()));
        }
        if !intent.tools.contains_key(&step.tool) {
            return Err(PlannerError::UnknownTool {
                step: step.id.clone(),
                tool: step.tool.clone(),
            });
        }
    }

    let mut plan = Plan::new();
    plan.name = intent.name;
    plan.tools = intent.tools;
    plan.outputs = intent.outputs;
    plan.nodes = intent
        .steps
        .into_iter()
        .map(compile_step)
        .collect::<Result<_, _>>()?;

    Ok(plan)
}

fn compile_step(step: IntentStep) -> Result<Node, PlannerError> {
    let mut depends_on = BTreeSet::new();
    for dependency in step.after {
        if dependency == step.id {
            return Err(PlannerError::SelfReference(step.id));
        }
        depends_on.insert(dependency);
    }
    let refs = collect_refs(&step.input).map_err(|error| PlannerError::InvalidRef {
        step: step.id.clone(),
        message: error.to_string(),
    })?;
    for value_ref in refs {
        if value_ref.node() == step.id {
            return Err(PlannerError::SelfReference(step.id));
        }
        depends_on.insert(value_ref.node().to_owned());
    }

    Ok(Node {
        id: step.id,
        tool: step.tool,
        input: step.input,
        depends_on: depends_on.into_iter().collect(),
        for_each: None,
        when: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::Effects;

    use super::*;

    fn tools() -> BTreeMap<ToolName, ToolSpec> {
        [(
            "echo".to_owned(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        )]
        .into()
    }

    #[test]
    fn compiles_intent_refs_into_dependencies() {
        let mut intent = IntentPlan::new().with_name("lookup");
        intent.tools = tools();
        intent
            .steps
            .push(IntentStep::new("user", "echo").with_input(json!({ "id": "u_1" })));
        intent.steps.push(
            IntentStep::new("profile", "echo")
                .with_input(json!({ "user": { "$ref": "user.output.id" } })),
        );

        let plan = compile_intent(intent).unwrap();

        assert_eq!(plan.name.as_deref(), Some("lookup"));
        assert_eq!(plan.nodes[1].depends_on, vec!["user"]);
    }

    #[test]
    fn merges_explicit_and_ref_dependencies() {
        let mut intent = IntentPlan::new();
        intent.tools = tools();
        intent.steps.push(IntentStep::new("audit", "echo"));
        intent.steps.push(IntentStep::new("user", "echo"));
        intent.steps.push(
            IntentStep::new("summary", "echo")
                .with_input(json!({ "user": { "$ref": "user.output" } }))
                .after(["audit"]),
        );

        let plan = compile_intent(intent).unwrap();

        assert_eq!(plan.nodes[2].depends_on, vec!["audit", "user"]);
    }

    #[test]
    fn rejects_unknown_versions() {
        let mut intent = IntentPlan::new();
        intent.version = "future".into();

        assert_eq!(
            compile_intent(intent).unwrap_err(),
            PlannerError::UnsupportedVersion("future".into())
        );
    }

    #[test]
    fn rejects_duplicate_steps_with_context() {
        let mut intent = IntentPlan::new();
        intent.tools = tools();
        intent.steps.push(IntentStep::new("same", "echo"));
        intent.steps.push(IntentStep::new("same", "echo"));

        assert_eq!(
            compile_intent(intent).unwrap_err(),
            PlannerError::DuplicateStep("same".into())
        );
    }

    #[test]
    fn rejects_unknown_tools_with_step_context() {
        let mut intent = IntentPlan::new();
        intent.steps.push(IntentStep::new("a", "missing"));

        assert_eq!(
            compile_intent(intent).unwrap_err(),
            PlannerError::UnknownTool {
                step: "a".into(),
                tool: "missing".into()
            }
        );
    }

    #[test]
    fn rejects_self_references() {
        let mut intent = IntentPlan::new();
        intent.tools = tools();
        intent
            .steps
            .push(IntentStep::new("loop", "echo").with_input(json!({
                "value": { "$ref": "loop.output" }
            })));

        assert_eq!(
            compile_intent(intent).unwrap_err(),
            PlannerError::SelfReference("loop".into())
        );
    }

    #[test]
    fn rejects_invalid_step_ids() {
        let mut intent = IntentPlan::new();
        intent.tools = tools();
        intent.steps.push(IntentStep::new("a.b", "echo"));

        assert!(matches!(
            compile_intent(intent).unwrap_err(),
            PlannerError::InvalidStepId { .. }
        ));
    }
}
