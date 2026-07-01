//! Validates every example JSON against the public schemas, so examples and
//! schemas cannot drift apart silently.

use std::path::Path;

use serde_json::Value;

use crate::{display, read_to_string};

pub(crate) fn validate_examples(root: &Path) -> Result<(), String> {
    let schemas = Schemas::load(root)?;
    let examples_dir = root.join("examples");
    let entries = std::fs::read_dir(&examples_dir)
        .map_err(|error| format!("failed to read {}: {error}", display(&examples_dir)))?;

    let mut checked = 0;
    let mut errors = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") || name.contains("config")
        {
            continue;
        }

        let value: Value = serde_json::from_str(&read_to_string(&path)?)
            .map_err(|error| format!("{}: invalid JSON: {error}", display(&path)))?;
        let (label, validator) = schemas.for_document(&value);
        if let Err(error) = validator.validate(&value) {
            errors.push(format!(
                "{} does not match the {label} schema: {error}",
                display(&path)
            ));
        }
        checked += 1;
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    println!("xtask examples: {checked} examples validated");
    Ok(())
}

struct Schemas {
    plan: jsonschema::Validator,
    intent: jsonschema::Validator,
    recipe: jsonschema::Validator,
}

impl Schemas {
    fn load(root: &Path) -> Result<Self, String> {
        let load = |name: &str| -> Result<jsonschema::Validator, String> {
            let path = root.join("schemas").join(name);
            let schema: Value = serde_json::from_str(&read_to_string(&path)?)
                .map_err(|error| format!("{name}: invalid JSON: {error}"))?;
            jsonschema::validator_for(&schema)
                .map_err(|error| format!("{name}: schema does not compile: {error}"))
        };
        Ok(Self {
            plan: load("plan.schema.json")?,
            intent: load("intent.schema.json")?,
            recipe: load("recipe.schema.json")?,
        })
    }

    fn for_document(&self, value: &Value) -> (&'static str, &jsonschema::Validator) {
        if value.get("steps").is_some() {
            ("intent", &self.intent)
        } else if value.get("recipe").is_some() {
            ("recipe", &self.recipe)
        } else {
            ("plan", &self.plan)
        }
    }
}
