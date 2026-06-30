use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub type NodeId = String;
pub type ToolName = String;
pub type Resource = String;

pub const CURRENT_VERSION: &str = "0";
pub const REF_KEY: &str = "$ref";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub version: String,
    #[serde(default)]
    pub tools: BTreeMap<ToolName, ToolSpec>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub outputs: BTreeMap<String, ValueRef>,
}

impl Plan {
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION.to_owned(),
            tools: BTreeMap::new(),
            nodes: Vec::new(),
            outputs: BTreeMap::new(),
        }
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
}

impl ToolSpec {
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
            effects: None,
        }
    }

    pub fn with_effects(mut self, effects: Effects) -> Self {
        self.effects = Some(effects);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effects {
    #[serde(default)]
    pub pure: bool,
    #[serde(default)]
    pub reads: BTreeSet<Resource>,
    #[serde(default)]
    pub writes: BTreeSet<Resource>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub cacheable: bool,
    #[serde(default)]
    pub batchable: bool,
    #[serde(default)]
    pub commutative: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
}

impl Effects {
    pub fn read_only(resources: impl IntoIterator<Item = impl Into<Resource>>) -> Self {
        Self {
            reads: resources.into_iter().map(Into::into).collect(),
            idempotent: true,
            cacheable: true,
            ..Self::default()
        }
    }

    pub fn pure() -> Self {
        Self {
            pure: true,
            idempotent: true,
            cacheable: true,
            commutative: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    #[serde(default)]
    pub retryable_errors: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub tool: ToolName,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub depends_on: Vec<NodeId>,
}

impl Node {
    pub fn new(id: impl Into<NodeId>, tool: impl Into<ToolName>) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            input: Value::Object(Default::default()),
            depends_on: Vec::new(),
        }
    }

    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ValueRef {
    node: NodeId,
    path: Vec<String>,
}

impl ValueRef {
    pub fn new(node: impl Into<NodeId>, path: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            node: node.into(),
            path: path.into_iter().map(Into::into).collect(),
        }
    }

    pub fn output(node: impl Into<NodeId>) -> Self {
        Self {
            node: node.into(),
            path: Vec::new(),
        }
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn path(&self) -> &[String] {
        &self.path
    }
}

impl fmt::Display for ValueRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.output", self.node)?;
        for segment in &self.path {
            write!(f, ".{segment}")?;
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

        match (node, output) {
            (Some(node), Some("output")) => Ok(Self::new(node, parts)),
            _ => Err(RefParseError::Invalid(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RefParseError {
    #[error("invalid value reference '{0}', expected '<node>.output[.<path>...]'")]
    Invalid(String),
}

pub fn collect_refs(value: &Value) -> Result<Vec<ValueRef>, RefParseError> {
    let mut refs = Vec::new();
    collect_refs_inner(value, &mut refs)?;
    Ok(refs)
}

fn collect_refs_inner(value: &Value, refs: &mut Vec<ValueRef>) -> Result<(), RefParseError> {
    match value {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(Value::String(raw_ref)) = map.get(REF_KEY)
            {
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
            vec![ValueRef::new("order", ["id"]), ValueRef::output("user"),]
        );
    }

    #[test]
    fn rejects_invalid_nested_refs() {
        let value = json!({ "bad": { "$ref": "missing-output" } });

        assert!(collect_refs(&value).is_err());
    }

    #[test]
    fn serializes_refs_as_strings() {
        let value = serde_json::to_value(ValueRef::new("a", ["b"])).unwrap();

        assert_eq!(value, json!("a.output.b"));
    }
}
