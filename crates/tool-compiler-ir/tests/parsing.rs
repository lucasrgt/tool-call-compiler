//! Property tests over the reference grammar and collectors.

use proptest::prelude::*;
use serde_json::{Value, json};
use tool_compiler_ir::{ValueRef, collect_refs};

/// Node ids: anything non-empty without dots.
fn arb_node_id() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_~-]{1,12}"
}

/// Path segments: arbitrary short strings including dots and tildes, which
/// must survive the escaping round-trip.
fn arb_segment() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_.~-]{1,10}"
}

fn arb_json(depth: u32) -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|n| json!(n)),
        "[a-zA-Z0-9$.{}~]{0,12}".prop_map(Value::String),
    ];
    leaf.prop_recursive(depth, 64, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::btree_map("[a-zA-Z$]{1,6}", inner, 0..4)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
    .boxed()
}

proptest! {
    /// Display → parse is the identity for any node id + path segments,
    /// including segments containing dots and tildes.
    #[test]
    fn value_refs_round_trip(node in arb_node_id(), path in proptest::collection::vec(arb_segment(), 0..4)) {
        let value_ref = ValueRef::new(&node, path.clone());
        let rendered = value_ref.to_string();
        let parsed: ValueRef = rendered.parse().expect("rendered refs parse");

        prop_assert_eq!(parsed.node(), node.as_str());
        prop_assert_eq!(parsed.path(), path.as_slice());
    }

    /// The collector never panics on arbitrary JSON — it either returns
    /// refs or a structured error.
    #[test]
    fn collect_refs_never_panics(value in arb_json(4)) {
        let _ = collect_refs(&value);
    }

    /// Parsing arbitrary strings never panics.
    #[test]
    fn ref_parsing_never_panics(raw in "\\PC{0,24}") {
        let _ = raw.parse::<ValueRef>();
    }
}
