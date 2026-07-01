//! Helpers for adapters that spawn external processes.

/// Returns the candidate program names to try when spawning `command`.
///
/// On Windows, commands installed by package managers are frequently launcher
/// shims (`npx.cmd`, `node.exe`, ...) that `std::process::Command` does not
/// resolve from a bare name. The returned list starts with the literal
/// command and, on Windows, appends `.cmd`, `.exe`, and `.bat` variants when
/// the name does not already carry an extension. On other platforms the list
/// is just the literal command.
pub fn command_candidates(command: &str) -> Vec<String> {
    let mut candidates = vec![command.to_owned()];
    #[cfg(windows)]
    {
        let lowered = command.to_ascii_lowercase();
        if !lowered.ends_with(".cmd") && !lowered.ends_with(".exe") && !lowered.ends_with(".bat") {
            candidates.push(format!("{command}.cmd"));
            candidates.push(format!("{command}.exe"));
            candidates.push(format!("{command}.bat"));
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_includes_the_literal_command() {
        assert_eq!(command_candidates("node")[0], "node");
    }

    #[cfg(windows)]
    #[test]
    fn windows_appends_launcher_shims() {
        let candidates = command_candidates("npx");

        assert!(candidates.contains(&"npx.cmd".to_owned()));
        assert!(candidates.contains(&"npx.exe".to_owned()));
        assert!(candidates.contains(&"npx.bat".to_owned()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_keeps_explicit_extensions_as_is() {
        assert_eq!(command_candidates("node.exe"), vec!["node.exe".to_owned()]);
    }
}
