//! High-level recipes lowered into executable plans.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tool_compiler_ir::{CURRENT_VERSION, ITEM_KEY, Node, Plan, ToolName, ToolSpec, ValueRef};

use crate::PlannerError;

/// A named, optionally parameterized recipe with its tool declarations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipePlan {
    /// Recipe IR version; must equal [`CURRENT_VERSION`].
    pub version: String,
    /// Optional human label carried into the compiled plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Declared parameters with default values; a `null` default marks the
    /// parameter as required. `{"$param": "name"}` placeholders inside the
    /// recipe are substituted at compile time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
    /// Tools referenced by the expanded nodes.
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    /// The execution shape.
    pub recipe: Recipe,
    /// Named outputs of the compiled plan.
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl RecipePlan {
    /// Creates a recipe plan at [`CURRENT_VERSION`].
    pub fn new(recipe: Recipe) -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            name: None,
            params: BTreeMap::new(),
            tools: BTreeMap::new(),
            recipe,
            outputs: BTreeMap::new(),
        }
    }

    /// Sets the human-readable name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Supported recipe shapes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recipe {
    /// One compact intent becomes many independent calls.
    FanOut(FanOutRecipe),
    /// Fan-out plus an aggregation call over every result.
    MapReduce(MapReduceRecipe),
    /// A sequential chain where each step consumes the previous output.
    Pipeline(PipelineRecipe),
}

/// Fan one tool out over items known at compile time (`items`) or over an
/// upstream array resolved at runtime (`items_ref`, which lowers to a
/// `for_each` node).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FanOutRecipe {
    /// Tool to call per item.
    pub tool: ToolName,
    /// Compile-time items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Value>,
    /// Runtime source of items (`<node>.output[.path]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_ref: Option<ValueRef>,
    /// Prefix for generated node ids.
    #[serde(default = "default_node_prefix")]
    pub node_prefix: String,
    /// When set, each item is wrapped as `{input_key: item}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_key: Option<String>,
}

impl FanOutRecipe {
    /// Fan out over compile-time items.
    pub fn new(tool: impl Into<ToolName>, items: impl IntoIterator<Item = Value>) -> Self {
        Self {
            tool: tool.into(),
            items: items.into_iter().collect(),
            items_ref: None,
            node_prefix: default_node_prefix(),
            input_key: None,
        }
    }

    /// Fan out over a runtime array.
    pub fn over_ref(tool: impl Into<ToolName>, items_ref: ValueRef) -> Self {
        Self {
            tool: tool.into(),
            items: Vec::new(),
            items_ref: Some(items_ref),
            node_prefix: default_node_prefix(),
            input_key: None,
        }
    }

    /// Sets the generated node id prefix.
    pub fn with_node_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.node_prefix = prefix.into();
        self
    }

    /// Wraps each item under `key` in the tool input.
    pub fn with_input_key(mut self, key: impl Into<String>) -> Self {
        self.input_key = Some(key.into());
        self
    }
}

/// Fan-out plus a reduce call receiving every mapped output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapReduceRecipe {
    /// The fan-out half.
    #[serde(flatten)]
    pub map: FanOutRecipe,
    /// Tool receiving the mapped outputs.
    pub reduce_tool: ToolName,
    /// Input key under which the mapped outputs are passed to the reducer.
    #[serde(default = "default_reduce_key")]
    pub reduce_input_key: String,
}

/// A sequential chain of calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineRecipe {
    /// Chain steps, in order.
    pub steps: Vec<PipelineStep>,
    /// Prefix for generated node ids.
    #[serde(default = "default_node_prefix")]
    pub node_prefix: String,
}

/// One pipeline step. Inside `input`, `{"$prev": "<path>"}` markers resolve
/// to the previous step's output (at the optional path).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineStep {
    /// Tool the step calls.
    pub tool: ToolName,
    /// Input template.
    #[serde(default)]
    pub input: Value,
}

fn default_node_prefix() -> String {
    "item_".into()
}

fn default_reduce_key() -> String {
    "items".into()
}

/// Compiles a recipe using only its declared parameter defaults.
pub fn compile_recipe(recipe_plan: RecipePlan) -> Result<Plan, PlannerError> {
    compile_recipe_with_params(recipe_plan, BTreeMap::new())
}

/// Compiles a recipe, substituting `{"$param": ...}` placeholders from
/// `values` merged over the declared defaults.
pub fn compile_recipe_with_params(
    recipe_plan: RecipePlan,
    values: BTreeMap<String, Value>,
) -> Result<Plan, PlannerError> {
    if recipe_plan.version != CURRENT_VERSION {
        return Err(PlannerError::UnsupportedVersion(recipe_plan.version));
    }

    let mut params = recipe_plan.params.clone();
    for (name, value) in values {
        if !params.contains_key(&name) {
            return Err(PlannerError::UnknownParam(name));
        }
        params.insert(name, value);
    }
    for (name, value) in &params {
        if value.is_null() {
            return Err(PlannerError::MissingParam(name.clone()));
        }
    }

    let recipe = substitute_params(recipe_plan.recipe, &params)?;

    let mut plan = Plan::new();
    plan.name = recipe_plan.name;
    plan.tools = recipe_plan.tools;
    plan.outputs = recipe_plan.outputs;
    plan.nodes = match recipe {
        Recipe::FanOut(fan_out) => compile_fan_out(&fan_out)?,
        Recipe::MapReduce(map_reduce) => compile_map_reduce(&map_reduce)?,
        Recipe::Pipeline(pipeline) => compile_pipeline(&pipeline)?,
    };
    Ok(plan)
}

fn compile_fan_out(recipe: &FanOutRecipe) -> Result<Vec<Node>, PlannerError> {
    match (&recipe.items_ref, recipe.items.is_empty()) {
        (Some(items_ref), _) => {
            let template = match &recipe.input_key {
                Some(key) => json!({ key: { ITEM_KEY: "" } }),
                None => json!({ ITEM_KEY: "" }),
            };
            Ok(vec![
                Node::new(format!("{}each", recipe.node_prefix), recipe.tool.clone())
                    .with_input(template)
                    .with_for_each(items_ref.clone()),
            ])
        }
        (None, true) => Err(PlannerError::EmptyFanOut),
        (None, false) => Ok(recipe
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                Node::new(
                    format!("{}{}", recipe.node_prefix, index + 1),
                    recipe.tool.clone(),
                )
                .with_input(wrap_item(recipe.input_key.as_deref(), item.clone()))
            })
            .collect()),
    }
}

fn compile_map_reduce(recipe: &MapReduceRecipe) -> Result<Vec<Node>, PlannerError> {
    let mut nodes = compile_fan_out(&recipe.map)?;
    let reduce_input = if nodes.len() == 1 && nodes[0].for_each.is_some() {
        // Dynamic fan-out: the single node's output is already the array.
        json!({ &recipe.reduce_input_key: { "$ref": format!("{}.output", nodes[0].id) } })
    } else {
        let refs: Vec<Value> = nodes
            .iter()
            .map(|node| json!({ "$ref": format!("{}.output", node.id) }))
            .collect();
        json!({ &recipe.reduce_input_key: refs })
    };
    nodes.push(
        Node::new(
            format!("{}reduce", recipe.map.node_prefix),
            recipe.reduce_tool.clone(),
        )
        .with_input(reduce_input),
    );
    Ok(nodes)
}

fn compile_pipeline(recipe: &PipelineRecipe) -> Result<Vec<Node>, PlannerError> {
    if recipe.steps.is_empty() {
        return Err(PlannerError::EmptyPipeline);
    }

    let mut nodes = Vec::with_capacity(recipe.steps.len());
    for (index, step) in recipe.steps.iter().enumerate() {
        let id = format!("{}{}", recipe.node_prefix, index + 1);
        let input = match index.checked_sub(1) {
            Some(previous) => {
                let previous_id = format!("{}{}", recipe.node_prefix, previous + 1);
                rewrite_prev_markers(step.input.clone(), &previous_id)
            }
            None => step.input.clone(),
        };
        nodes.push(Node::new(id, step.tool.clone()).with_input(input));
    }
    Ok(nodes)
}

/// Replaces `{"$prev": "<path>"}` markers with `{"$ref": "<prev>.output[.path]"}`.
fn rewrite_prev_markers(value: Value, previous_id: &str) -> Value {
    match value {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(path)) = map.get("$prev")
            {
                let reference = if path.is_empty() {
                    format!("{previous_id}.output")
                } else {
                    format!("{previous_id}.output.{path}")
                };
                return json!({ "$ref": reference });
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, rewrite_prev_markers(value, previous_id)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| rewrite_prev_markers(item, previous_id))
                .collect(),
        ),
        other => other,
    }
}

fn wrap_item(input_key: Option<&str>, item: Value) -> Value {
    let Some(key) = input_key else {
        return item;
    };
    let mut value = Map::new();
    value.insert(key.to_owned(), item);
    Value::Object(value)
}

/// Substitutes `{"$param": "name"}` placeholders throughout a recipe.
fn substitute_params(
    recipe: Recipe,
    params: &BTreeMap<String, Value>,
) -> Result<Recipe, PlannerError> {
    if params.is_empty() {
        return Ok(recipe);
    }
    let raw = serde_json::to_value(&recipe).expect("recipes serialize");
    let substituted = substitute_value(raw, params)?;
    Ok(serde_json::from_value(substituted).expect("substitution preserves the recipe shape"))
}

fn substitute_value(
    value: Value,
    params: &BTreeMap<String, Value>,
) -> Result<Value, PlannerError> {
    match value {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(name)) = map.get("$param")
            {
                return params
                    .get(name)
                    .cloned()
                    .ok_or_else(|| PlannerError::UnknownParam(name.clone()));
            }
            let mut out = Map::with_capacity(map.len());
            for (key, value) in map {
                out.insert(key, substitute_value(value, params)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .into_iter()
            .map(|item| substitute_value(item, params))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use tool_compiler_ir::Effects;

    use super::*;

    fn echo_tools() -> BTreeMap<ToolName, ToolSpec> {
        [(
            "echo".to_owned(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        )]
        .into()
    }

    #[test]
    fn compiles_static_fan_out() {
        let mut recipe = RecipePlan::new(Recipe::FanOut(
            FanOutRecipe::new("echo", [json!("a"), json!("b")])
                .with_node_prefix("read_")
                .with_input_key("path"),
        ))
        .with_name("read docs");
        recipe.tools = echo_tools();
        recipe
            .outputs
            .insert("first".into(), ValueRef::output("read_1"));

        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(plan.name.as_deref(), Some("read docs"));
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[0].id, "read_1");
        assert_eq!(plan.nodes[0].input, json!({ "path": "a" }));
    }

    #[test]
    fn compiles_dynamic_fan_out_to_for_each() {
        let mut recipe = RecipePlan::new(Recipe::FanOut(
            FanOutRecipe::over_ref("echo", ValueRef::new("search", ["hits"]))
                .with_input_key("doc"),
        ));
        recipe.tools = echo_tools();

        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(
            plan.nodes[0].for_each,
            Some(ValueRef::new("search", ["hits"]))
        );
        assert_eq!(plan.nodes[0].input, json!({ "doc": { "$item": "" } }));
    }

    #[test]
    fn rejects_empty_fan_out() {
        let recipe = RecipePlan::new(Recipe::FanOut(FanOutRecipe::new("echo", [])));

        assert_eq!(
            compile_recipe(recipe).unwrap_err(),
            PlannerError::EmptyFanOut
        );
    }

    #[test]
    fn compiles_map_reduce_with_static_items() {
        let mut recipe = RecipePlan::new(Recipe::MapReduce(MapReduceRecipe {
            map: FanOutRecipe::new("echo", [json!(1), json!(2)]).with_node_prefix("m_"),
            reduce_tool: "echo".into(),
            reduce_input_key: "items".into(),
        }));
        recipe.tools = echo_tools();

        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(plan.nodes.len(), 3);
        assert_eq!(plan.nodes[2].id, "m_reduce");
        assert_eq!(
            plan.nodes[2].input,
            json!({ "items": [{ "$ref": "m_1.output" }, { "$ref": "m_2.output" }] })
        );
    }

    #[test]
    fn compiles_pipelines_with_prev_markers() {
        let mut recipe = RecipePlan::new(Recipe::Pipeline(PipelineRecipe {
            steps: vec![
                PipelineStep {
                    tool: "echo".into(),
                    input: json!({ "q": "start" }),
                },
                PipelineStep {
                    tool: "echo".into(),
                    input: json!({ "from": { "$prev": "q" }, "all": { "$prev": "" } }),
                },
            ],
            node_prefix: "step_".into(),
        }));
        recipe.tools = echo_tools();

        let plan = compile_recipe(recipe).unwrap();

        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(
            plan.nodes[1].input,
            json!({
                "from": { "$ref": "step_1.output.q" },
                "all": { "$ref": "step_1.output" }
            })
        );
    }

    #[test]
    fn substitutes_declared_params() {
        let mut recipe = RecipePlan::new(Recipe::FanOut(
            FanOutRecipe::new("echo", [json!({ "$param": "query" })]).with_input_key("q"),
        ));
        recipe.tools = echo_tools();
        recipe.params.insert("query".into(), json!("default"));

        let defaulted = compile_recipe(recipe.clone()).unwrap();
        assert_eq!(defaulted.nodes[0].input, json!({ "q": "default" }));

        let supplied = compile_recipe_with_params(
            recipe,
            [("query".to_owned(), json!("override"))].into(),
        )
        .unwrap();
        assert_eq!(supplied.nodes[0].input, json!({ "q": "override" }));
    }

    #[test]
    fn required_params_must_be_supplied() {
        let mut recipe = RecipePlan::new(Recipe::FanOut(
            FanOutRecipe::new("echo", [json!({ "$param": "query" })]),
        ));
        recipe.params.insert("query".into(), Value::Null);

        assert_eq!(
            compile_recipe(recipe.clone()).unwrap_err(),
            PlannerError::MissingParam("query".into())
        );
        assert_eq!(
            compile_recipe_with_params(
                recipe,
                [("other".to_owned(), json!(1))].into()
            )
            .unwrap_err(),
            PlannerError::UnknownParam("other".into())
        );
    }
}
