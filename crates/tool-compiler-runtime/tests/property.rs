//! Property tests for the invariant that protects the whole product:
//! for any valid plan over deterministic tools, optimized parallel
//! execution and serial execution produce the same declared outputs.

use std::collections::BTreeMap;

use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::{Value, json};
use tool_compiler_ir::{Effects, Node, Plan, ToolSpec, ValueRef};
use tool_compiler_runtime::{
    BatchInput, BatchOutput, RunConfig, RunStatus, Runtime, ToolExecutionError, ToolExecutor,
};

/// Deterministic executor: the output is a pure function of tool + input.
struct DeterministicExecutor;

fn deterministic_output(tool: &str, input: &Value) -> Value {
    json!({ "tool": tool, "echo": input })
}

#[async_trait]
impl ToolExecutor for DeterministicExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        Ok(deterministic_output(tool, &input))
    }

    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        Ok(inputs
            .into_iter()
            .map(|input| BatchOutput {
                output: deterministic_output(tool, &input.input),
                node: input.node,
            })
            .collect())
    }
}

/// Tool palette: pure/batchable, reads over three resources, writes over
/// three resources — enough to exercise dedup, batching, locks, and
/// ordering.
fn tools() -> BTreeMap<String, ToolSpec> {
    let mut tools = BTreeMap::new();
    tools.insert(
        "pure".to_owned(),
        ToolSpec::new("test").with_effects(Effects {
            batchable: true,
            ..Effects::pure()
        }),
    );
    for index in 0..3 {
        tools.insert(
            format!("read{index}"),
            ToolSpec::new("test").with_effects(Effects::read_only([format!("res:{index}")])),
        );
        tools.insert(
            format!("write{index}"),
            ToolSpec::new("test").with_effects(Effects {
                writes: [format!("res:{index}")].into_iter().collect(),
                idempotent: true,
                ..Effects::default()
            }),
        );
    }
    tools
}

#[derive(Clone, Debug)]
struct NodeSpecArb {
    tool_index: usize,
    payload: u8,
    /// Indexes (into earlier nodes) for explicit ordering edges.
    after: Vec<usize>,
    /// Index of an earlier node whose output feeds this input, if any.
    reference: Option<usize>,
}

fn arb_plan() -> impl Strategy<Value = Plan> {
    let node = (
        0usize..7,
        any::<u8>(),
        proptest::collection::vec(0usize..100, 0..3),
        proptest::option::of(0usize..100),
    )
        .prop_map(|(tool_index, payload, after, reference)| NodeSpecArb {
            tool_index,
            payload,
            after,
            reference,
        });

    proptest::collection::vec(node, 1..12).prop_map(|specs| {
        let tool_names: Vec<String> = tools().keys().cloned().collect();
        let mut plan = Plan::new();
        plan.tools = tools();

        for (index, spec) in specs.iter().enumerate() {
            let id = format!("n{index}");
            let tool = tool_names[spec.tool_index % tool_names.len()].clone();
            let mut input = json!({ "payload": spec.payload });
            if index > 0
                && let Some(reference) = spec.reference
            {
                let target = format!("n{}", reference % index);
                input["from"] = json!({ "$ref": format!("{target}.output") });
            }
            let mut node = Node::new(id, tool).with_input(input);
            if index > 0 {
                node.depends_on = spec
                    .after
                    .iter()
                    .map(|after| format!("n{}", after % index))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
            }
            plan.nodes.push(node);
        }

        // Declare every node as an output so nothing is DCE-pruned and the
        // comparison covers the full graph.
        for index in 0..specs.len() {
            let id = format!("n{index}");
            plan.outputs.insert(id.clone(), ValueRef::output(&id));
        }
        plan
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// THE invariant: optimized parallel execution is observationally
    /// equivalent to serial execution for deterministic tools.
    #[test]
    fn optimized_run_equals_serial_run(plan in arb_plan()) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async {
            let executor = Runtime::single_adapter("test", DeterministicExecutor);

            let compiled = executor
                .run_with(plan.clone(), RunConfig::new().with_cache(false))
                .await
                .expect("compiled run");
            let serial = executor
                .run_serial_with(plan, RunConfig::new().with_cache(false))
                .await
                .expect("serial run");

            prop_assert_eq!(compiled.status, RunStatus::Success);
            prop_assert_eq!(serial.status, RunStatus::Success);
            prop_assert_eq!(&compiled.outputs, &serial.outputs);
            Ok(())
        })?;
    }
}

#[tokio::test]
async fn stress_wide_fan_out_of_a_thousand_nodes() {
    let mut plan = Plan::new();
    plan.tools.insert(
        "read".into(),
        ToolSpec::new("test").with_effects(Effects {
            batchable: true,
            ..Effects::pure()
        }),
    );
    for index in 0..1000 {
        plan.nodes
            .push(Node::new(format!("n{index}"), "read").with_input(json!({ "i": index })));
    }

    let runtime = Runtime::single_adapter("test", DeterministicExecutor);
    let result = runtime.run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(result.metrics.nodes_succeeded, 1000);
}

#[tokio::test]
async fn stress_unbalanced_chains_overlap() {
    // A long chain next to a wide independent shelf: dependency-driven
    // scheduling must finish both without stalls.
    let mut plan = Plan::new();
    plan.tools.insert(
        "pure".into(),
        ToolSpec::new("test").with_effects(Effects::pure()),
    );
    for index in 0..50 {
        let mut node = Node::new(format!("chain{index}"), "pure").with_input(json!({ "i": index }));
        if index > 0 {
            node.depends_on = vec![format!("chain{}", index - 1)];
        }
        plan.nodes.push(node);
    }
    for index in 0..200 {
        plan.nodes
            .push(Node::new(format!("wide{index}"), "pure").with_input(json!({ "w": index })));
    }

    let runtime = Runtime::single_adapter("test", DeterministicExecutor);
    let result = runtime
        .run_with(plan, RunConfig::new().with_max_concurrency(16))
        .await
        .unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(result.metrics.nodes_total, 250);
    assert_eq!(result.metrics.nodes_failed, 0);
}
