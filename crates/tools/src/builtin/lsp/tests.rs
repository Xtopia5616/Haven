use super::{
    LspOperation, document_position, operation_specs, redact_lsp_file_uris, resolve_lsp_path,
    select_lsp_server,
};
use haven_common::config::LspServerConfig;
use serde_json::json;
use std::path::Path;

#[test]
fn converts_one_based_utf16_positions_to_lsp_coordinates() {
    let source = "first\nA🦀B\n";
    assert_eq!(document_position(source, 2, 4).unwrap(), (1, 3));
    assert!(document_position(source, 2, 5).is_err());
    assert!(document_position(source, 0, 1).is_err());
}

#[test]
fn lsp_paths_reject_parent_traversal_and_resolve_relative_to_workspace() {
    let root = Path::new("C:\\work");
    assert_eq!(
        resolve_lsp_path("src\\main.rs", root).unwrap(),
        root.join("src\\main.rs")
    );
    assert!(resolve_lsp_path("..\\private.rs", root).is_err());
}

#[test]
fn lsp_server_selection_matches_case_insensitively_and_prefers_compound_extensions() {
    let servers = vec![
        LspServerConfig {
            name: "typescript".into(),
            command: "ts-language-server".into(),
            extensions: vec![".ts".into()],
            language_id: "typescript".into(),
            enabled: true,
            args: Vec::new(),
        },
        LspServerConfig {
            name: "declaration".into(),
            command: "declaration-language-server".into(),
            extensions: vec![".d.ts".into()],
            language_id: "typescript".into(),
            enabled: true,
            args: Vec::new(),
        },
    ];

    assert_eq!(
        select_lsp_server(servers.clone(), Path::new("types.d.TS"))
            .unwrap()
            .name,
        "declaration"
    );
    assert_eq!(
        select_lsp_server(servers, Path::new("source.TS"))
            .unwrap()
            .name,
        "typescript"
    );
}

#[test]
fn lsp_results_use_workspace_relative_uris_and_redact_external_file_uris() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let directory =
        haven_platform::filesystem::DirectoryCapability::open(workspace.path()).unwrap();
    let inside_uri = url::Url::from_file_path(workspace.path().join("src/lib.rs"))
        .unwrap()
        .to_string();
    let outside_uri = url::Url::from_file_path(outside.path().join("secret.rs"))
        .unwrap()
        .to_string();
    let mut result = json!({
        "locations": [{"uri": inside_uri}, {"targetUri": outside_uri}]
    });

    redact_lsp_file_uris(&mut result, &directory);

    assert_eq!(
        result["locations"][0]["uri"],
        Path::new("src").join("lib.rs").to_string_lossy().as_ref()
    );
    assert!(result["locations"][1]["targetUri"].is_null());
}

#[test]
fn lsp_specs_declare_host_execution_and_opaque_network_policy() {
    for spec in operation_specs() {
        assert_eq!(spec.policy.risk_level, crate::RiskLevel::High);
        assert_eq!(spec.policy.network_access, crate::NetworkAccess::Opaque);
        assert_eq!(
            spec.policy.execution_boundary,
            crate::ExecutionBoundary::HostProcess
        );
        assert_eq!(
            spec.policy.idempotency,
            crate::OperationIdempotency::NonIdempotent
        );
        assert!(spec.schema["additionalProperties"] == json!(false));
        assert!(spec.prompt.when_to_use.len() > 8);
    }
    assert_eq!(LspOperation::parse("hover").unwrap(), LspOperation::Hover);
    assert!(LspOperation::parse("formatting").is_err());
}
