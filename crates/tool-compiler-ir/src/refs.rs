//! Value references, reference collection, and literal escaping.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::NodeId;

/// Input key that marks a JSON object as a reference: `{"$ref": "node.output.path"}`.
pub const REF_KEY: &str = "$ref";

/// Input key that marks a JSON object as literal data: `{"$literal": <value>}`.
///
/// Everything under `$literal` is passed through verbatim and is never
/// interpreted as a reference, which is how a plan can carry payloads that
/// legitimately contain `$ref`-shaped objects (JSON Schemas, for example).
pub const LITERAL_KEY: &str = "$literal";

/// Validates a node id for use in plans and references.
///
/// Node ids must be non-empty and must not contain `.` (the reference
/// separator) so that `<node>.output.<path>` strings stay unambiguous.
pub fn validate_node_id(id: &str) -> Result<(), RefParseError> {
    if id.is_empty() {
        return Err(RefParseError::EmptyNodeId);
    }
    if id.contains('.') {
        return Err(RefParseError::NodeIdContainsDot(id.to_owned()));
    }
    Ok(())
}

/// A parsed reference to another node's output, with an optional path.
///
/// The string form is `<node>.output[.<segment>...]`. Path segments address
/// object keys or array indexes. Segments are escaped JSON-Pointer style:
/// `~1` stands for a literal `.` and `~0` for a literal `~`, so keys that
/// contain dots remain addressable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ValueRef {
    node: NodeId,
    path: Vec<String>,
}

impl ValueRef {
    /// Creates a reference to `node`'s output at `path`.
    pub fn new(node: impl Into<NodeId>, path: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            node: node.into(),
            path: path.into_iter().map(Into::into).collect(),
        }
    }

    /// Creates a reference to `node`'s whole output.
    pub fn output(node: impl Into<NodeId>) -> Self {
        Self {
            node: node.into(),
            path: Vec::new(),
        }
    }

    /// The referenced node id.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// The path segments below the node output (unescaped).
    pub fn path(&self) -> &[String] {
        &self.path
    }
}

fn escape_segment(segment: &str) -> std::borrow::Cow<'_, str> {
    if segment.contains(['~', '.']) {
        segment.replace('~', "~0").replace('.', "~1").into()
    } else {
        segment.into()
    }
}

fn unescape_segment(segment: &str) -> String {
    if segment.contains('~') {
        segment.replace("~1", ".").replace("~0", "~")
    } else {
        segment.to_owned()
    }
}

impl fmt::Display for ValueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.output", self.node)?;
        for segment in &self.path {
            write!(f, ".{}", escape_segment(segment))?;
        }
        Ok(())
    }
}

impl From<ValueRef> for String {
    fn from(value: ValueRef) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for ValueRef {
    type Error = RefParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl FromStr for ValueRef {
    type Err = RefParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let node = parts.next().filter(|part| !part.is_empty());
        let output = parts.next();

        let (Some(node), Some("output")) = (node, output) else {
            return Err(RefParseError::Invalid(value.to_owned()));
        };

        let mut path = Vec::new();
        for segment in parts {
            if segment.is_empty() {
                return Err(RefParseError::EmptySegment(value.to_owned()));
            }
            path.push(unescape_segment(segment));
        }

        Ok(Self {
            node: node.to_owned(),
            path,
        })
    }
}

/// Errors produced while parsing or collecting value references.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RefParseError {
    /// The string does not match `<node>.output[.<path>...]`.
    #[error("invalid value reference '{0}', expected '<node>.output[.<path>...]'")]
    Invalid(String),
    /// The reference contains an empty path segment (for example a trailing dot).
    #[error("value reference '{0}' contains an empty path segment")]
    EmptySegment(String),
    /// A `$ref` object carries a non-string value.
    #[error("'$ref' value must be a string, found {0}")]
    NonStringRef(String),
    /// A `$ref` object carries extra keys next to `$ref`.
    #[error(
        "object mixes '$ref' with other keys ({0}); wrap literal data in {{\"$literal\": ...}}"
    )]
    RefWithExtraKeys(String),
    /// A node id is empty.
    #[error("node id must not be empty")]
    EmptyNodeId,
    /// A node id contains the reserved `.` separator.
    #[error("node id '{0}' must not contain '.'")]
    NodeIdContainsDot(String),
}

/// Collects every `$ref` reference inside `value`.
///
/// Objects shaped `{"$ref": "..."}` are parsed as references. Objects that
/// carry a `$ref` key in any other shape (non-string value, or mixed with
/// other keys) are rejected instead of being silently treated as data — an
/// LLM-authored plan with a malformed reference should fail validation, not
/// quietly send the marker object to a tool. Anything under a
/// [`LITERAL_KEY`] object is skipped verbatim.
pub fn collect_refs(value: &Value) -> Result<Vec<ValueRef>, RefParseError> {
    let mut refs = Vec::new();
    collect_refs_inner(value, &mut refs)?;
    Ok(refs)
}

fn collect_refs_inner(value: &Value, refs: &mut Vec<ValueRef>) -> Result<(), RefParseError> {
    match value {
        Value::Object(map) => {
            if map.len() == 1 && map.contains_key(LITERAL_KEY) {
                return Ok(());
            }

            if let Some(raw_ref) = map.get(REF_KEY) {
                if map.len() > 1 {
                    let other_keys = map
                        .keys()
                        .filter(|key| key.as_str() != REF_KEY)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(RefParseError::RefWithExtraKeys(other_keys));
                }
                let Value::String(raw_ref) = raw_ref else {
                    return Err(RefParseError::NonStringRef(raw_ref.to_string()));
                };
                refs.push(raw_ref.parse()?);
                return Ok(());
            }

            for value in map.values() {
                collect_refs_inner(value, refs)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs_inner(item, refs)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_value_refs() {
        let value_ref: ValueRef = "user.output.profile.name".parse().unwrap();

        assert_eq!(value_ref.node(), "user");
        assert_eq!(value_ref.path(), ["profile", "name"]);
        assert_eq!(value_ref.to_string(), "user.output.profile.name");
    }

    #[test]
    fn rejects_invalid_refs() {
        assert!("user.profile.name".parse::<ValueRef>().is_err());
        assert!(".output".parse::<ValueRef>().is_err());
    }

    #[test]
    fn rejects_empty_path_segments() {
        assert_eq!(
            "a.output.".parse::<ValueRef>().unwrap_err(),
            RefParseError::EmptySegment("a.output.".into())
        );
        assert!("a.output.x..y".parse::<ValueRef>().is_err());
    }

    #[test]
    fn escapes_dotted_path_segments() {
        let value_ref = ValueRef::new("node", ["a.b", "c~d"]);
        let rendered = value_ref.to_string();

        assert_eq!(rendered, "node.output.a~1b.c~0d");
        assert_eq!(rendered.parse::<ValueRef>().unwrap(), value_ref);
    }

    #[test]
    fn validates_node_ids() {
        assert!(validate_node_id("fine_id-1").is_ok());
        assert_eq!(
            validate_node_id("").unwrap_err(),
            RefParseError::EmptyNodeId
        );
        assert_eq!(
            validate_node_id("a.b").unwrap_err(),
            RefParseError::NodeIdContainsDot("a.b".into())
        );
    }

    #[test]
    fn collects_nested_refs() {
        let value = json!({
            "user": { "$ref": "user.output" },
            "items": [
                { "id": { "$ref": "order.output.id" } }
            ]
        });

        let mut refs = collect_refs(&value).unwrap();
        refs.sort();

        assert_eq!(
            refs,
            vec![ValueRef::new("order", ["id"]), ValueRef::output("user")]
        );
    }

    #[test]
    fn rejects_invalid_nested_refs() {
        let value = json!({ "bad": { "$ref": "missing-output" } });

        assert!(collect_refs(&value).is_err());
    }

    #[test]
    fn rejects_non_string_ref_values() {
        let error = collect_refs(&json!({ "bad": { "$ref": 123 } })).unwrap_err();

        assert!(matches!(error, RefParseError::NonStringRef(_)));
    }

    #[test]
    fn rejects_refs_mixed_with_other_keys() {
        let error =
            collect_refs(&json!({ "bad": { "$ref": "a.output", "extra": 1 } })).unwrap_err();

        assert!(matches!(error, RefParseError::RefWithExtraKeys(keys) if keys == "extra"));
    }

    #[test]
    fn literal_objects_shield_ref_shaped_data() {
        let value = json!({
            "schema": { "$literal": { "$ref": "#/definitions/user", "extra": 1 } }
        });

        assert_eq!(collect_refs(&value).unwrap(), Vec::<ValueRef>::new());
    }

    #[test]
    fn serializes_refs_as_strings() {
        let value = serde_json::to_value(ValueRef::new("a", ["b"])).unwrap();

        assert_eq!(value, json!("a.output.b"));
    }
}
