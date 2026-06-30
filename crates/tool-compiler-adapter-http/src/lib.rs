use tool_compiler_ir::{Effects, ToolSpec};

pub const ADAPTER: &str = "http";

pub fn read_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

pub fn pure_endpoint() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::pure())
}

pub fn write_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_endpoint_declares_read_only_effects() {
        let spec = read_endpoint(["api:user"]);
        let effects = spec.effects.unwrap();

        assert!(effects.reads.contains("api:user"));
        assert!(effects.writes.is_empty());
        assert!(effects.cacheable);
    }

    #[test]
    fn write_endpoint_declares_writes() {
        let spec = write_endpoint(["api:user"]);
        let effects = spec.effects.unwrap();

        assert!(effects.writes.contains("api:user"));
        assert!(!effects.cacheable);
    }

    #[test]
    fn pure_endpoint_declares_pure_effects() {
        let spec = pure_endpoint();
        let effects = spec.effects.unwrap();

        assert!(effects.pure);
        assert!(effects.commutative);
    }
}
