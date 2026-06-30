use tool_compiler_ir::{Effects, ToolSpec};

pub const ADAPTER: &str = "mcp";

pub fn pure_tool() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::pure())
}

pub fn read_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

pub fn effectful_tool(
    reads: impl IntoIterator<Item = impl Into<String>>,
    writes: impl IntoIterator<Item = impl Into<String>>,
) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        reads: reads.into_iter().map(Into::into).collect(),
        writes: writes.into_iter().map(Into::into).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_tool_is_cacheable() {
        let effects = pure_tool().effects.unwrap();

        assert!(effects.pure);
        assert!(effects.cacheable);
    }

    #[test]
    fn effectful_tool_declares_reads_and_writes() {
        let effects = effectful_tool(["fs:read"], ["fs:write"]).effects.unwrap();

        assert!(effects.reads.contains("fs:read"));
        assert!(effects.writes.contains("fs:write"));
        assert!(!effects.cacheable);
    }

    #[test]
    fn read_tool_declares_read_only_effects() {
        let effects = read_tool(["mcp:resource"]).effects.unwrap();

        assert!(effects.reads.contains("mcp:resource"));
        assert!(effects.writes.is_empty());
    }
}
