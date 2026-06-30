use serde::{Deserialize, Serialize};
use tool_compiler_ir::{Effects, Node, Plan, ToolSpec, ValueRef};

use crate::{Runtime, RuntimeError, ToolExecutor, ToolRegistry};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceReport {
    pub passed: bool,
    pub checks: Vec<ConformanceCheck>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

pub async fn check_adapter_conformance(
    adapter: impl Into<String>,
    executor: impl ToolExecutor + 'static,
) -> ConformanceReport {
    let adapter = adapter.into();
    let runtime =
        Runtime::from_registry(ToolRegistry::new().with_adapter(adapter.clone(), executor));
    let checks = vec![
        check_echo_round_trip(&runtime, &adapter).await,
        check_batch_contract(&runtime, &adapter).await,
        check_error_propagation(&runtime, &adapter).await,
    ];

    ConformanceReport {
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}

async fn check_echo_round_trip(runtime: &Runtime, adapter: &str) -> ConformanceCheck {
    let mut plan = Plan::new();
    plan.tools.insert(
        "echo".into(),
        ToolSpec::new(adapter).with_effects(Effects::pure()),
    );
    plan.nodes
        .push(Node::new("echo", "echo").with_input(serde_json::json!({ "ok": true })));
    plan.outputs.insert("echo".into(), ValueRef::output("echo"));

    match runtime.run(plan).await {
        Ok(result) if result.outputs["echo"] == serde_json::json!({ "ok": true }) => {
            conformance_pass("echo_round_trip")
        }
        Ok(_) => conformance_fail("echo_round_trip", "echo output did not match input"),
        Err(error) => conformance_fail("echo_round_trip", error.to_string()),
    }
}

async fn check_batch_contract(runtime: &Runtime, adapter: &str) -> ConformanceCheck {
    let mut plan = Plan::new();
    plan.tools.insert(
        "echo".into(),
        ToolSpec::new(adapter).with_effects(Effects {
            batchable: true,
            ..Effects::pure()
        }),
    );
    plan.nodes
        .push(Node::new("a", "echo").with_input(serde_json::json!({ "id": "a" })));
    plan.nodes
        .push(Node::new("b", "echo").with_input(serde_json::json!({ "id": "b" })));
    plan.outputs.insert("a".into(), ValueRef::output("a"));
    plan.outputs.insert("b".into(), ValueRef::output("b"));

    match runtime.run(plan).await {
        Ok(result)
            if result.optimization.batch_groups.len() == 1
                && result.outputs["a"] == serde_json::json!({ "id": "a" })
                && result.outputs["b"] == serde_json::json!({ "id": "b" }) =>
        {
            conformance_pass("batch_contract")
        }
        Ok(_) => conformance_fail("batch_contract", "batch result did not match inputs"),
        Err(error) => conformance_fail("batch_contract", error.to_string()),
    }
}

async fn check_error_propagation(runtime: &Runtime, adapter: &str) -> ConformanceCheck {
    let mut plan = Plan::new();
    plan.tools.insert(
        "fail".into(),
        ToolSpec::new(adapter).with_effects(Effects::pure()),
    );
    plan.nodes.push(Node::new("bad", "fail"));

    match runtime.run(plan).await {
        Err(RuntimeError::Tool { .. }) => conformance_pass("error_propagation"),
        Err(error) => conformance_fail("error_propagation", error.to_string()),
        Ok(_) => conformance_fail("error_propagation", "fail tool returned success"),
    }
}

fn conformance_pass(name: &str) -> ConformanceCheck {
    ConformanceCheck {
        name: name.to_owned(),
        passed: true,
        message: "passed".into(),
    }
}

fn conformance_fail(name: &str, message: impl Into<String>) -> ConformanceCheck {
    ConformanceCheck {
        name: name.to_owned(),
        passed: false,
        message: message.into(),
    }
}
