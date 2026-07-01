//! Release consistency checks: version numbers and changelog coverage.

use std::path::Path;

use serde_json::Value;

use crate::read_to_string;

pub(crate) fn release_check(root: &Path) -> Result<(), String> {
    let workspace_version = workspace_version(root)?;
    let sdk_version = sdk_version(root)?;
    let mut errors = Vec::new();

    if workspace_version != sdk_version {
        errors.push(format!(
            "version drift: Cargo workspace is {workspace_version}, sdk/node package.json is {sdk_version}"
        ));
    }

    let changelog = read_to_string(&root.join("CHANGELOG.md"))?;
    let heading = format!("## {workspace_version}");
    if !changelog.contains(&heading) {
        errors.push(format!(
            "CHANGELOG.md has no '{heading}' section for the current version"
        ));
    }

    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    println!("xtask release-check: version {workspace_version} consistent");
    Ok(())
}

fn workspace_version(root: &Path) -> Result<String, String> {
    let manifest = read_to_string(&root.join("Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        })
        .ok_or_else(|| "workspace Cargo.toml has no version".to_owned())
}

fn sdk_version(root: &Path) -> Result<String, String> {
    let package: Value = serde_json::from_str(&read_to_string(
        &root.join("sdk").join("node").join("package.json"),
    )?)
    .map_err(|error| format!("sdk/node/package.json: {error}"))?;
    package["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "sdk/node/package.json has no version".to_owned())
}
