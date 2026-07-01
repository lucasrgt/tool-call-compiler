//! The compiler as an MCP stdio server.
//!
//! Tools: `run_compiled_tool_graph`, `run_compiled_tool_intent`,
//! `run_compiled_tool_recipe` (with `params`), and `explain_tool_plan`
//! (validate + optimization report, no execution). `tools/list` embeds the
//! full public JSON Schemas so a model authoring plans sees the exact
//! shapes.
//!
//! Robustness rules: malformed JSON gets a `-32700` response instead of
//! killing the server; requests are handled concurrently; notifications are
//! ignored; `ping` is supported; failures of the *plan* come back as tool
//! results with `isError: true` and the partial `RunResult` in
//! `structuredContent` — never as protocol errors.
//!
//! Recursion guard: the server reads [`DEPTH_ENV_VAR`] (set by the MCP
//! adapter when it spawns children) and refuses `tools/call` beyond depth 2.

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tool_compiler_adapter_mcp::DEPTH_ENV_VAR;
use tool_compiler_ir::Plan;
use tool_compiler_optimizer::explain;
use tool_compiler_planner::{IntentPlan, RecipePlan, compile_intent, compile_recipe_with_params};
use tool_compiler_runtime::{ErrorMode, ResultMode, RunConfig, Runtime};

use crate::CliError;

/// Model-visible tool names served (and blocked inside the runtime to stop
/// self-recursion through a misconfigured adapter).
pub const SERVED_TOOLS: [&str; 4] = [
    "run_compiled_tool_graph",
    "run_compiled_tool_intent",
    "run_compiled_tool_recipe",
    "explain_tool_plan",
];

/// Maximum nested compiler depth allowed for `tools/call`.
pub const MAX_DEPTH: u32 = 2;

/// Server state shared across concurrent requests.
pub struct ServeState {
    runtime: Runtime,
    depth: u32,
}

impl ServeState {
    /// Wraps a runtime, reading the composition depth from the environment.
    pub fn new(runtime: Runtime) -> Self {
        let depth = std::env::var(DEPTH_ENV_VAR)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Self { runtime, depth }
    }

    #[cfg(test)]
    fn with_depth(runtime: Runtime, depth: u32) -> Self {
        Self { runtime, depth }
    }
}

/// Serves MCP over stdio until stdin closes.
pub async fn serve(runtime: Runtime) -> Result<(), CliError> {
    let state = Arc::new(ServeState::new(runtime));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = receiver.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdout.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    eprintln!("tool-compiler serve-mcp ready (depth {})", state.depth);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let state = state.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            if let Some(response) = handle_line(&state, &line).await
                && let Ok(serialized) = serde_json::to_string(&response)
            {
                let _ = sender.send(serialized);
            }
        });
    }

    drop(sender);
    let _ = writer.await;
    Ok(())
}

/// Handles one raw input line; malformed JSON produces a parse-error
/// response instead of terminating the server.
pub async fn handle_line(state: &ServeState, line: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(line) {
        Ok(message) => handle_message(state, message).await,
        Err(error) => Some(jsonrpc_error(
            Value::Null,
            -32700,
            format!("parse error: {error}"),
        )),
    }
}

/// Handles one JSON-RPC message.
pub async fn handle_message(state: &ServeState, message: Value) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");

    match method {
        "initialize" => Some(jsonrpc_result(
            id,
            initialize_result(message.get("params")),
        )),
        "ping" => Some(jsonrpc_result(id, json!({}))),
        "tools/list" => Some(jsonrpc_result(id, tools_list())),
        "tools/call" => Some(jsonrpc_result(id, call_tool(state, &message).await)),
        other => Some(jsonrpc_error(
            id,
            -32601,
            format!("unknown method '{other}'"),
        )),
    }
}

fn initialize_result(params: Option<&Value>) -> Value {
    // Echo the client's revision when it looks like a dated MCP revision;
    // otherwise answer with ours.
    let requested = params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .filter(|version| version.len() == 10 && version.chars().filter(|c| *c == '-').count() == 2);
    json!({
        "protocolVersion": requested.unwrap_or(tool_compiler_adapter_mcp::PROTOCOL_VERSION),
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "tool-call-compiler",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn schemas() -> &'static (Value, Value, Value) {
    static SCHEMAS: OnceLock<(Value, Value, Value)> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let parse = |raw: &str| serde_json::from_str(raw).expect("bundled schemas are valid JSON");
        (
            parse(include_str!("../../../schemas/plan.schema.json")),
            parse(include_str!("../../../schemas/intent.schema.json")),
            parse(include_str!("../../../schemas/recipe.schema.json")),
        )
    })
}

fn run_options_properties() -> Value {
    json!({
        "result_mode": {
            "description": "How much detail the result carries.",
            "enum": ["full", "compact"]
        },
        "on_error": {
            "description": "Failure handling: stop scheduling (fail_fast) or keep independent branches running (continue).",
            "enum": ["fail_fast", "continue"]
        }
    })
}

fn tools_list() -> Value {
    let (plan_schema, intent_schema, recipe_schema) = schemas();
    let with_options = |required: &str, schema: &Value| {
        let mut properties = json!({ required: schema });
        if let (Value::Object(properties_map), Value::Object(options)) =
            (&mut properties, run_options_properties())
        {
            properties_map.extend(options);
        }
        json!({
            "type": "object",
            "required": [required],
            "properties": properties
        })
    };

    json!({
        "tools": [{
            "name": "run_compiled_tool_graph",
            "title": "Run compiled tool graph",
            "description": "Execute a tool-call-compiler Plan as one composite tool call; partial results survive node failures.",
            "inputSchema": with_options("plan", plan_schema)
        }, {
            "name": "run_compiled_tool_intent",
            "title": "Run compiled tool intent",
            "description": "Compile an agent intent (refs + after ordering) into a Plan and execute it as one composite tool call.",
            "inputSchema": with_options("intent", intent_schema)
        }, {
            "name": "run_compiled_tool_recipe",
            "title": "Run compiled tool recipe",
            "description": "Compile a high-level recipe (fan_out, map_reduce, pipeline; optional params) into a Plan and execute it.",
            "inputSchema": {
                "type": "object",
                "required": ["recipe"],
                "properties": {
                    "recipe": recipe_schema,
                    "params": { "type": "object", "description": "Values for declared recipe parameters." },
                    "result_mode": { "enum": ["full", "compact"] },
                    "on_error": { "enum": ["fail_fast", "continue"] }
                }
            }
        }, {
            "name": "explain_tool_plan",
            "title": "Explain a tool plan",
            "description": "Validate and optimize a Plan WITHOUT executing it: layers, optimization report, and why nodes were not parallelized.",
            "inputSchema": {
                "type": "object",
                "required": ["plan"],
                "properties": { "plan": plan_schema }
            }
        }]
    })
}

async fn call_tool(state: &ServeState, message: &Value) -> Value {
    if state.depth >= MAX_DEPTH {
        return tool_failure(format!(
            "maximum compiler composition depth ({MAX_DEPTH}) exceeded; \
             flatten the plan instead of nesting compilers"
        ));
    }

    match try_call_tool(state, message).await {
        Ok(result) => result,
        Err(message) => tool_failure(message),
    }
}

async fn try_call_tool(state: &ServeState, message: &Value) -> Result<Value, String> {
    let params = message
        .get("params")
        .ok_or("tools/call missing params".to_owned())?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call missing name".to_owned())?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let plan: Plan = match name {
        "run_compiled_tool_graph" | "explain_tool_plan" => {
            let plan = arguments
                .get("plan")
                .cloned()
                .ok_or_else(|| format!("{name} requires arguments.plan"))?;
            serde_json::from_value(plan).map_err(|error| error.to_string())?
        }
        "run_compiled_tool_intent" => {
            let intent = arguments
                .get("intent")
                .cloned()
                .ok_or("run_compiled_tool_intent requires arguments.intent".to_owned())?;
            let intent: IntentPlan =
                serde_json::from_value(intent).map_err(|error| error.to_string())?;
            compile_intent(intent).map_err(|error| error.to_string())?
        }
        "run_compiled_tool_recipe" => {
            let recipe = arguments
                .get("recipe")
                .cloned()
                .ok_or("run_compiled_tool_recipe requires arguments.recipe".to_owned())?;
            let recipe: RecipePlan =
                serde_json::from_value(recipe).map_err(|error| error.to_string())?;
            let params = match arguments.get("params") {
                Some(Value::Object(map)) => map
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                _ => Default::default(),
            };
            compile_recipe_with_params(recipe, params).map_err(|error| error.to_string())?
        }
        other => return Err(format!("unknown tool '{other}'")),
    };

    if name == "explain_tool_plan" {
        let report = explain(plan).map_err(|error| error.to_string())?;
        return Ok(json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "{} layers; {} diagnostics",
                    report.layers.len(),
                    report.diagnostics.len()
                )
            }],
            "structuredContent": report,
            "isError": false
        }));
    }

    let config = run_config_from(&arguments);
    let result = state
        .runtime
        .run_with(plan, config)
        .await
        .map_err(|error| error.to_string())?;

    let is_error = result.status == tool_compiler_runtime::RunStatus::Failed;
    let summary = format!(
        "status={:?} nodes={} ok={} failed={} skipped={} wall={}ms",
        result.status,
        result.metrics.nodes_total,
        result.metrics.nodes_succeeded,
        result.metrics.nodes_failed,
        result.metrics.nodes_skipped,
        result.metrics.wall_ms,
    );
    Ok(json!({
        "content": [{ "type": "text", "text": summary }],
        "structuredContent": result,
        "isError": is_error
    }))
}

fn run_config_from(arguments: &Value) -> RunConfig {
    let mut config = RunConfig::new();
    if arguments.get("result_mode").and_then(Value::as_str) == Some("compact") {
        config = config.with_result_mode(ResultMode::Compact);
    }
    if arguments.get("on_error").and_then(Value::as_str) == Some("continue") {
        config = config.with_on_error(ErrorMode::Continue);
    }
    config
}

fn tool_failure(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn jsonrpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

#[cfg(test)]
mod tests {
    use tool_compiler_runtime::Runtime;

    use super::*;
    use crate::local::LocalExecutor;

    fn state() -> ServeState {
        ServeState::new(Runtime::single_adapter("local", LocalExecutor))
    }

    fn plan_json() -> Value {
        json!({
            "version": "0",
            "tools": {
                "echo": {
                    "adapter": "local",
                    "effects": { "pure": true, "idempotent": true, "cacheable": true }
                }
            },
            "nodes": [{ "id": "a", "tool": "echo", "input": { "v": 1 } }],
            "outputs": { "a": "a.output" }
        })
    }

    fn call(name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
    }

    #[tokio::test]
    async fn malformed_json_yields_parse_error_response() {
        let response = handle_line(&state(), "{not json").await.unwrap();

        assert_eq!(response["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let response = handle_message(
            &state(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;

        assert!(response.is_none());
    }

    #[tokio::test]
    async fn ping_and_initialize_are_answered() {
        let ping = handle_message(&state(), json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .await
            .unwrap();
        assert_eq!(ping["result"], json!({}));

        let init = handle_message(
            &state(),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26" }
            }),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    }

    #[tokio::test]
    async fn tools_list_embeds_the_full_schemas() {
        let response = handle_message(
            &state(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await
        .unwrap();

        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(
            tools[0]["inputSchema"]["properties"]["plan"]["title"],
            "Tool Call Compiler Plan"
        );
    }

    #[tokio::test]
    async fn runs_plans_and_returns_structured_results() {
        let response = handle_message(&state(), call("run_compiled_tool_graph", json!({ "plan": plan_json() })))
            .await
            .unwrap();

        let result = &response["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["outputs"]["a"]["v"], 1);
    }

    #[tokio::test]
    async fn plan_failures_are_tool_errors_with_partial_results() {
        let plan = json!({
            "version": "0",
            "tools": { "fail": { "adapter": "local", "effects": { "pure": true } } },
            "nodes": [{ "id": "bad", "tool": "fail" }]
        });
        let response = handle_message(&state(), call("run_compiled_tool_graph", json!({ "plan": plan })))
            .await
            .unwrap();

        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert!(result["structuredContent"]["errors"]["bad"]["message"].is_string());
    }

    #[tokio::test]
    async fn runs_recipes_with_params() {
        let recipe = json!({
            "version": "0",
            "params": { "query": null },
            "tools": { "echo": { "adapter": "local", "effects": { "pure": true } } },
            "recipe": {
                "kind": "fan_out",
                "tool": "echo",
                "items": [{ "$param": "query" }],
                "input_key": "q"
            }
        });
        let response = handle_message(
            &state(),
            call(
                "run_compiled_tool_recipe",
                json!({ "recipe": recipe, "params": { "query": "hello" } }),
            ),
        )
        .await
        .unwrap();

        let result = &response["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(
            result["structuredContent"]["node_outputs"]["item_1"]["q"],
            "hello"
        );
    }

    #[tokio::test]
    async fn explains_without_executing() {
        let response = handle_message(&state(), call("explain_tool_plan", json!({ "plan": plan_json() })))
            .await
            .unwrap();

        let result = &response["result"];
        assert_eq!(result["isError"], false);
        assert!(result["structuredContent"]["optimization"]["passes"].is_array());
    }

    #[tokio::test]
    async fn depth_guard_refuses_nested_composition() {
        let deep = ServeState::with_depth(
            Runtime::single_adapter("local", LocalExecutor),
            MAX_DEPTH,
        );
        let response = handle_message(&deep, call("run_compiled_tool_graph", json!({ "plan": plan_json() })))
            .await
            .unwrap();

        assert_eq!(response["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_tools_and_methods_error() {
        let unknown_tool = handle_message(&state(), call("nope", json!({})))
            .await
            .unwrap();
        assert_eq!(unknown_tool["result"]["isError"], true);

        let unknown_method = handle_message(
            &state(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "bogus" }),
        )
        .await
        .unwrap();
        assert_eq!(unknown_method["error"]["code"], -32601);
    }
}
