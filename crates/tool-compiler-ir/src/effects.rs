//! Tool effect declarations and per-node resource resolution.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Resource;

/// Declared semantic properties of a tool call.
///
/// Effects are what make optimization legal: the compiler only reorders,
/// parallelizes, batches, caches, deduplicates, or retries a call when its
/// declared effects prove the transformation safe. A missing or vacuous
/// declaration blocks those transformations instead of permitting them.
///
/// `reads` and `writes` name abstract resources. Entries may contain
/// `{placeholders}` resolved from each node's input (see
/// [`resolve_resource_template`]), so one tool can declare per-call
/// granularity such as `file:{path}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effects {
    /// No external side effects; output depends only on input.
    #[serde(default)]
    pub pure: bool,
    /// Abstract resources read by the call.
    #[serde(default)]
    pub reads: BTreeSet<Resource>,
    /// Abstract resources written by the call.
    #[serde(default)]
    pub writes: BTreeSet<Resource>,
    /// Retrying the call does not duplicate external effects.
    #[serde(default)]
    pub idempotent: bool,
    /// The output may be memoized for a cache key.
    #[serde(default)]
    pub cacheable: bool,
    /// Multiple calls of this tool may be joined into one batch dispatch.
    #[serde(default)]
    pub batchable: bool,
    /// Calls of this tool may be reordered or overlapped with each other
    /// even when their writes touch the same resource.
    #[serde(default)]
    pub commutative: bool,
    /// Default execution deadline for one call, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Retry policy applied when a call fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
}

impl Effects {
    /// Read-only effects over `resources`: idempotent and cacheable.
    pub fn read_only(resources: impl IntoIterator<Item = impl Into<Resource>>) -> Self {
        Self {
            reads: resources.into_iter().map(Into::into).collect(),
            idempotent: true,
            cacheable: true,
            ..Self::default()
        }
    }

    /// Pure effects: no side effects, idempotent, cacheable, commutative.
    pub fn pure() -> Self {
        Self {
            pure: true,
            idempotent: true,
            cacheable: true,
            commutative: true,
            ..Self::default()
        }
    }

    /// Whether the declaration contradicts itself (`pure` with writes).
    pub fn is_contradictory(&self) -> bool {
        self.pure && !self.writes.is_empty()
    }

    /// Whether the declaration says anything about side effects.
    ///
    /// An `Effects` value that declares neither purity nor any read/write
    /// resource is *vacuous*: it asserts safety without knowledge. The
    /// compiler treats vacuous effects like missing effects and refuses to
    /// parallelize on their basis.
    pub fn declares_side_effects(&self) -> bool {
        self.pure || !self.reads.is_empty() || !self.writes.is_empty()
    }
}

/// Retry policy for failed calls.
///
/// Retries only run when the tool's effects make them safe (`idempotent` or
/// `pure`). An error is considered retryable when the adapter marked it
/// `retryable: true`, or — if the adapter gave no verdict — when
/// `retryable_errors` is empty (any error) or matches the error's `code` or a
/// substring of its message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// Maximum total attempts, including the first call.
    pub max_attempts: u8,
    /// Error codes or message substrings that are retryable.
    #[serde(default)]
    pub retryable_errors: BTreeSet<String>,
    /// Base backoff in milliseconds; attempt `n` waits `backoff_ms * 2^(n-1)`
    /// plus jitter, capped at 5 seconds. Defaults to 100ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<u64>,
}

/// Resolves `{placeholder}` markers in a resource template against a node input.
///
/// Placeholders name a top-level input field or a dotted path into it
/// (`{path}`, `{user.id}`). They resolve only when the addressed value is a
/// scalar (string, number, or boolean). If any placeholder cannot be resolved
/// — the field is missing, non-scalar, or itself a `$ref` that is only known
/// at runtime — the template is returned unresolved, so every node of that
/// tool shares one conservative resource and serializes instead of racing.
pub fn resolve_resource_template(template: &str, input: &Value) -> Resource {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        let placeholder = &rest[start + 1..start + len];
        let Some(scalar) = lookup_scalar(input, placeholder) else {
            return template.to_owned();
        };
        out.push_str(&rest[..start]);
        out.push_str(&scalar);
        rest = &rest[start + len + 1..];
    }

    out.push_str(rest);
    out
}

fn lookup_scalar(input: &Value, path: &str) -> Option<String> {
    let mut current = input;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    match current {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pure_with_writes_is_contradictory() {
        let effects = Effects {
            writes: ["db:x"].into_iter().map(String::from).collect(),
            ..Effects::pure()
        };

        assert!(effects.is_contradictory());
        assert!(!Effects::pure().is_contradictory());
    }

    #[test]
    fn vacuous_effects_declare_nothing() {
        assert!(!Effects::default().declares_side_effects());
        assert!(
            !Effects {
                idempotent: true,
                batchable: true,
                ..Effects::default()
            }
            .declares_side_effects()
        );
        assert!(Effects::pure().declares_side_effects());
        assert!(Effects::read_only(["api:x"]).declares_side_effects());
    }

    #[test]
    fn resolves_templates_from_scalar_inputs() {
        let input = json!({ "path": "src/lib.rs", "user": { "id": 7 } });

        assert_eq!(
            resolve_resource_template("file:{path}", &input),
            "file:src/lib.rs"
        );
        assert_eq!(
            resolve_resource_template("user:{user.id}:profile", &input),
            "user:7:profile"
        );
        assert_eq!(resolve_resource_template("static", &input), "static");
    }

    #[test]
    fn unresolvable_templates_stay_conservative() {
        let input = json!({ "path": { "$ref": "other.output" }, "list": [1] });

        assert_eq!(
            resolve_resource_template("file:{path}", &input),
            "file:{path}"
        );
        assert_eq!(
            resolve_resource_template("x:{missing}", &input),
            "x:{missing}"
        );
        assert_eq!(resolve_resource_template("x:{list}", &input), "x:{list}");
    }

    #[test]
    fn rejects_unknown_effect_fields() {
        let error = serde_json::from_value::<Effects>(json!({ "purity": true })).unwrap_err();

        assert!(error.to_string().contains("purity"));
    }
}
