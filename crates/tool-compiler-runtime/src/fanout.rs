//! Dynamic fan-out execution for `for_each` nodes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tool_compiler_adapter_api::{BatchInput, ToolExecutionError};

use crate::dispatch::{DispatchContext, DispatchOutcome, PreparedMember, UnitSpec, elapsed_ms};
use crate::policy::{call_batch_with_policy, call_with_policy};
use crate::result::{TraceEvent, TraceStatus};

pub(crate) async fn dispatch_fan_out(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: PreparedMember,
    outcome: &mut DispatchOutcome,
) {
    let items = member.items.clone().unwrap_or_default();
    outcome.events.push(TraceEvent::new(
        &member.node,
        &spec.tool,
        TraceStatus::Started,
    ));
    let started = Instant::now();

    let mut item_inputs = Vec::with_capacity(items.len());
    for item in &items {
        match crate::refs::substitute_item(&member.input, item) {
            Ok(input) => item_inputs.push(input),
            Err(error) => {
                let error = ToolExecutionError::new(error.to_string()).with_code("invalid_item");
                fail_fan_out(&member, spec, error, started, outcome);
                return;
            }
        }
    }

    let mut results: Vec<Option<Value>> = vec![None; item_inputs.len()];
    let mut pending: Vec<usize> = Vec::new();
    if context.use_cache {
        for (index, input) in item_inputs.iter().enumerate() {
            match member.key(spec, input) {
                Some(key) => match context.cache.get(&key).await {
                    Some(cached) => {
                        outcome.cache_hits += 1;
                        results[index] = Some(cached.as_ref().clone());
                    }
                    None => pending.push(index),
                },
                None => pending.push(index),
            }
        }
    } else {
        pending = (0..item_inputs.len()).collect();
    }

    let run = if member.batchable && pending.len() > 1 {
        run_fan_out_batched(
            context,
            spec,
            &member,
            &item_inputs,
            &pending,
            &mut results,
            outcome,
        )
        .await
    } else {
        run_fan_out_sequential(
            context,
            spec,
            &member,
            &item_inputs,
            &pending,
            &mut results,
            outcome,
        )
        .await
    };
    if let Err(error) = run {
        fail_fan_out(&member, spec, error, started, outcome);
        return;
    }

    let output = Value::Array(results.into_iter().map(Option::unwrap_or_default).collect());
    outcome.events.push(
        TraceEvent::new(&member.node, &spec.tool, TraceStatus::Finished)
            .with_duration(elapsed_ms(started)),
    );
    outcome.results.push((member.node, Ok(output)));
}

async fn run_fan_out_batched(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    item_inputs: &[Value],
    pending: &[usize],
    results: &mut [Option<Value>],
    outcome: &mut DispatchOutcome,
) -> Result<(), ToolExecutionError> {
    for chunk in pending.chunks(member.batch_size.max(1)) {
        let inputs: Vec<BatchInput> = chunk
            .iter()
            .map(|index| BatchInput {
                node: index.to_string(),
                input: item_inputs[*index].clone(),
            })
            .collect();
        outcome.batch_dispatches += 1;
        let result =
            call_batch_with_policy(&context.executor, &spec.tool, &inputs, &member.policy).await;
        outcome.retries += result.retries.len();
        let outputs = result.result?;
        let mut by_index: BTreeMap<usize, Value> = outputs
            .into_iter()
            .filter_map(|output| Some((output.node.parse::<usize>().ok()?, output.output)))
            .collect();
        for index in chunk {
            let output = by_index.remove(index).ok_or_else(|| {
                ToolExecutionError::new(format!(
                    "batch call did not return output for item {index}"
                ))
                .with_code("batch_missing_output")
            })?;
            insert_item_cache(context, spec, member, &item_inputs[*index], &output).await;
            results[*index] = Some(output);
        }
    }
    Ok(())
}

async fn run_fan_out_sequential(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    item_inputs: &[Value],
    pending: &[usize],
    results: &mut [Option<Value>],
    outcome: &mut DispatchOutcome,
) -> Result<(), ToolExecutionError> {
    for index in pending {
        let result = call_with_policy(
            &context.executor,
            &spec.tool,
            &item_inputs[*index],
            &member.policy,
        )
        .await;
        outcome.retries += result.retries.len();
        let output = result.result.map_err(|error| {
            ToolExecutionError::new(format!("item {index}: {}", error.message))
                .with_code(error.code.unwrap_or_else(|| "item_failed".into()))
        })?;
        insert_item_cache(context, spec, member, &item_inputs[*index], &output).await;
        results[*index] = Some(output);
    }
    Ok(())
}

async fn insert_item_cache(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    input: &Value,
    output: &Value,
) {
    if context.use_cache
        && let Some(key) = member.key(spec, input)
    {
        context
            .cache
            .insert(key, Arc::new(output.clone()), member.reads.clone())
            .await;
    }
}

fn fail_fan_out(
    member: &PreparedMember,
    spec: &UnitSpec,
    error: ToolExecutionError,
    started: Instant,
    outcome: &mut DispatchOutcome,
) {
    outcome.events.push(
        TraceEvent::new(&member.node, &spec.tool, TraceStatus::Failed)
            .with_duration(elapsed_ms(started))
            .with_error(&error),
    );
    outcome.results.push((member.node.clone(), Err(error)));
}
