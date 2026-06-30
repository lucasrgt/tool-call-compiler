use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipePlan {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    pub recipe: Recipe,
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl RecipePlan {
    pub fn new(recipe: Recipe) -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            name: None,
            tools: BTreeMap::new(),
            recipe,
            outputs: BTreeMap::new(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recipe {
    FanOut(FanOutRecipe),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FanOutRecipe {
    pub tool: ToolName,
    pub items: Vec<Value>,
    #[serde(default = "default_node_prefix")]
    pub node_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_key: Option<String>,
}

impl FanOutRecipe {
    pub fn new(tool: impl Into<ToolName>, items: impl IntoIterator<Item = Value>) -> Self {
        Self {
            tool: tool.into(),
            items: items.into_iter().collect(),
            node_prefix: default_node_prefix(),
            input_key: None,
        }
    }

    pub fn with_node_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.node_prefix = prefix.into();
        self
    }

    pub fn with_input_key(mut self, key: impl Into<String>) -> Self {
        self.input_key = Some(key.into());
        self
    }
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

pub fn compile_recipe(recipe_plan: RecipePlan) -> Result<Plan, PlannerError> {
    if recipe_plan.version != CURRENT_VERSION {
        return Err(PlannerError::UnsupportedVersion(recipe_plan.version));
    }

    let mut plan = Plan::new();
    plan.tools = recipe_plan.tools;
    plan.outputs = recipe_plan.outputs;
    match recipe_plan.recipe {
        Recipe::FanOut(recipe) => {
            plan.nodes = recipe
                .items
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    Node::new(
                        format!("{}{}", recipe.node_prefix, index + 1),
                        recipe.tool.clone(),
                    )
                    .with_input(recipe_input(recipe.input_key.as_deref(), item))
                })
                .collect();
        }
    }
    Ok(plan)
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

fn default_node_prefix() -> String {
    "item_".into()
}

fn recipe_input(input_key: Option<&str>, item: Value) -> Value {
    let Some(key) = input_key else {
        return item;
    };
    let mut value = Map::new();
    value.insert(key.to_owned(), item);
    Value::Object(value)
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

    #[test]
    fn compiles_fan_out_recipe() {
        let mut recipe = RecipePlan::new(Recipe::FanOut(
            FanOutRecipe::new("echo", [json!("a"), json!("b")])
                .with_node_prefix("read_")
                .with_input_key("path"),
        ))
        .with_name("read docs");
        recipe.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        recipe
            .outputs
            .insert("first".into(), ValueRef::output("read_1"));

        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[0].id, "read_1");
        assert_eq!(plan.nodes[0].input, json!({ "path": "a" }));
        assert_eq!(plan.outputs["first"], ValueRef::output("read_1"));
    }

    #[test]
    fn recipe_name_is_metadata_only() {
        let recipe = RecipePlan::new(Recipe::FanOut(FanOutRecipe::new("echo", [json!("a")])))
            .with_name("lookup docs");

        let value = serde_json::to_value(&recipe).unwrap();
        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(value["name"], "lookup docs");
        assert_eq!(plan.nodes.len(), 1);
    }
}
