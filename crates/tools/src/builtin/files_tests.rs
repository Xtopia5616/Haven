#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn files_tool_with_registry(registry: ManagedAssetRegistry) -> FilesTool {
        let mut tool = FilesTool::default();
        let max_output_chars = tool.max_output_chars;
        tool.managed_assets = registry.clone();
        tool.media_tool = Some(Arc::new(MediaTool::new(
            None,
            registry,
            8 * 1024 * 1024,
            120,
            max_output_chars,
        )));
        tool
    }

    #[path = "../files_tests/core.rs"]
    mod core;
    #[path = "../files_tests/media_summary.rs"]
    mod media_summary;
    #[path = "../files_tests/contract.rs"]
    mod contract;
    #[path = "../files_tests/read.rs"]
    mod read;
    #[path = "../files_tests/mutations.rs"]
    mod mutations;
    #[path = "../files_tests/managed.rs"]
    mod managed;
}
