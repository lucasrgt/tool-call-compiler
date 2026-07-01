//! Compiler-opportunity mining over observed tool calls.
//!
//! Hosts feed the sequence of tool calls an agent actually made;
//! [`suggest_recipes`] finds runs of consecutive same-tool calls — the
//! real-world signal that a loop of model turns was spent on deterministic
//! orchestration — and proposes ready-to-fill [`RecipePlan`]s.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::recipes::{FanOutRecipe, Recipe, RecipePlan};

/// One tool call observed in an agent transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedCall {
    /// Tool name as the agent called it.
    pub tool: String,
    /// The call input.
    #[serde(default)]
    pub input: Value,
}

/// Kind of a mined suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    /// A run of same-tool calls that could be one fan-out recipe.
    FanOut,
}

/// A mined compiler opportunity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// Suggestion kind.
    pub kind: SuggestionKind,
    /// Tool the run called.
    pub tool: String,
    /// How many consecutive calls the run contained.
    pub occurrences: usize,
    /// Zero-based index of the first call of the run in the input slice.
    pub start_index: usize,
    /// A ready recipe carrying the observed inputs as items. The `tools`
    /// map is intentionally empty: the host must declare the tool's adapter
    /// and effects before running it.
    pub recipe: RecipePlan,
    /// Human-readable rationale.
    pub reason: String,
}

/// Default minimum run length worth suggesting.
pub const DEFAULT_MIN_OCCURRENCES: usize = 3;

/// Scans `calls` for runs of at least `min_occurrences` consecutive calls of
/// the same tool and returns one fan-out suggestion per run.
pub fn suggest_recipes(calls: &[ObservedCall], min_occurrences: usize) -> Vec<Suggestion> {
    let min_occurrences = min_occurrences.max(2);
    let mut suggestions = Vec::new();
    let mut index = 0;

    while index < calls.len() {
        let tool = &calls[index].tool;
        let mut end = index + 1;
        while end < calls.len() && &calls[end].tool == tool {
            end += 1;
        }

        let run = end - index;
        if run >= min_occurrences {
            let items: Vec<Value> = calls[index..end]
                .iter()
                .map(|call| call.input.clone())
                .collect();
            let recipe = RecipePlan::new(Recipe::FanOut(FanOutRecipe::new(tool.clone(), items)))
                .with_name(format!("{tool} fan-out ({run} calls)"));
            suggestions.push(Suggestion {
                kind: SuggestionKind::FanOut,
                tool: tool.clone(),
                occurrences: run,
                start_index: index,
                recipe,
                reason: format!(
                    "{run} consecutive '{tool}' calls; if they are independent, one \
                     fan-out recipe replaces {run} model-visible tool calls with one"
                ),
            });
        }
        index = end;
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(tool: &str, input: Value) -> ObservedCall {
        ObservedCall {
            tool: tool.into(),
            input,
        }
    }

    #[test]
    fn suggests_fan_out_for_consecutive_runs() {
        let calls = vec![
            call("read_file", json!({ "path": "a.md" })),
            call("read_file", json!({ "path": "b.md" })),
            call("read_file", json!({ "path": "c.md" })),
            call("write_file", json!({ "path": "out.md" })),
        ];

        let suggestions = suggest_recipes(&calls, 3);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].tool, "read_file");
        assert_eq!(suggestions[0].occurrences, 3);
        assert_eq!(suggestions[0].start_index, 0);
        let Recipe::FanOut(fan_out) = &suggestions[0].recipe.recipe else {
            panic!("expected fan-out recipe");
        };
        assert_eq!(fan_out.items.len(), 3);
    }

    #[test]
    fn short_runs_are_not_suggested() {
        let calls = vec![
            call("read_file", json!({})),
            call("read_file", json!({})),
            call("other", json!({})),
        ];

        assert!(suggest_recipes(&calls, 3).is_empty());
    }

    #[test]
    fn finds_multiple_runs() {
        let calls = vec![
            call("a", json!(1)),
            call("a", json!(2)),
            call("a", json!(3)),
            call("b", json!(1)),
            call("b", json!(2)),
            call("b", json!(3)),
        ];

        let suggestions = suggest_recipes(&calls, 3);

        assert_eq!(suggestions.len(), 2);
        assert_eq!(suggestions[1].tool, "b");
        assert_eq!(suggestions[1].start_index, 3);
    }
}
