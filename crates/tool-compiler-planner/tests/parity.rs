//! Golden parity fixtures shared with the TypeScript SDK.
//!
//! Every fixture under `/tests/parity` carries a source (`intent` or
//! `recipe` + optional `params`) and the exact `plan` both compilers must
//! produce. The TypeScript test suite executes the same files.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use tool_compiler_planner::{compile_intent, compile_recipe_with_params};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("parity")
}

#[test]
fn every_parity_fixture_compiles_to_its_expected_plan() {
    let mut checked = 0;

    for entry in std::fs::read_dir(fixtures_dir()).expect("parity fixtures directory exists") {
        let path = entry.expect("readable entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let fixture: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("fixture is readable"),
        )
        .expect("fixture is valid JSON");
        let name = fixture["name"].as_str().unwrap_or("unnamed").to_owned();
        let expected = fixture["plan"].clone();

        let compiled = if let Some(intent) = fixture.get("intent") {
            let intent = serde_json::from_value(intent.clone())
                .unwrap_or_else(|error| panic!("{name}: intent deserializes: {error}"));
            compile_intent(intent).unwrap_or_else(|error| panic!("{name}: compiles: {error}"))
        } else {
            let recipe = serde_json::from_value(fixture["recipe"].clone())
                .unwrap_or_else(|error| panic!("{name}: recipe deserializes: {error}"));
            let params: BTreeMap<String, Value> = fixture
                .get("params")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            compile_recipe_with_params(recipe, params)
                .unwrap_or_else(|error| panic!("{name}: compiles: {error}"))
        };

        let compiled = serde_json::to_value(&compiled).expect("plans serialize");
        assert_eq!(compiled, expected, "fixture '{name}' diverged");
        checked += 1;
    }

    assert!(checked >= 5, "expected at least 5 parity fixtures, found {checked}");
}
