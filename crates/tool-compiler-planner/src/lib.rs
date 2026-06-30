use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tool_compiler_ir::{
    CURRENT_VERSION, Node, NodeId, Plan, ToolName, ToolSpec, ValueRef, collect_refs,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntentPlan {
    pub version: String,
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    #[serde(default)]
    pub steps: Vec<IntentStep>,
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl IntentPlan {
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            tools: BTreeMap::new(),
            steps: Vec::new(),
            outputs: BTreeMap::new(),
        }
    }
}

impl Default for IntentPlan {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntentStep {
    pub id: NodeId,
    pub tool: ToolName,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub after: Vec<NodeId>,
}

impl IntentStep {
    pub fn new(id: impl Into<NodeId>, tool: impl Into<ToolName>) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            input: Value::Object(Default::default()),
            after: Vec::new(),
        }
    }

    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    pub fn after(mut self, dependencies: impl IntoIterator<Item = impl Into<NodeId>>) -> Self {
        self.after = dependencies.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlannerError {
    #[error("unsupported intent version '{0}'")]
    UnsupportedVersion(String),
    #[error(transparent)]
    InvalidRef(#[from] tool_compiler_ir::RefParseError),
}

pub fn compile_intent(intent: IntentPlan) -> Result<Plan, PlannerError> {
    if intent.version != CURRENT_VERSION {
        return Err(PlannerError::UnsupportedVersion(intent.version));
    }

    let mut plan = Plan::new();
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
    let mut depends_on = step.after.into_iter().collect::<BTreeSet<_>>();
    for value_ref in collect_refs(&step.input)? {
        depends_on.insert(value_ref.node().to_owned());
    }

    Ok(Node {
        id: step.id,
        tool: step.tool,
        input: step.input,
        depends_on: depends_on.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, ToolSpec};

    use super::*;

    #[test]
    fn compiles_intent_refs_into_dependencies() {
        let mut intent = IntentPlan::new();
        intent.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        intent
            .steps
            .push(IntentStep::new("user", "echo").with_input(json!({ "id": "u_1" })));
        intent.steps.push(
            IntentStep::new("profile", "echo")
                .with_input(json!({ "user": { "$ref": "user.output.id" } })),
        );

        let plan = compile_intent(intent).unwrap();

        assert_eq!(plan.nodes[1].depends_on, vec!["user"]);
    }

    #[test]
    fn merges_explicit_and_ref_dependencies() {
        let mut intent = IntentPlan::new();
        intent.steps.push(
            IntentStep::new("summary", "echo")
                .with_input(json!({ "user": { "$ref": "user.output" } }))
                .after(["audit"]),
        );

        let plan = compile_intent(intent).unwrap();

        assert_eq!(plan.nodes[0].depends_on, vec!["audit", "user"]);
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
}
