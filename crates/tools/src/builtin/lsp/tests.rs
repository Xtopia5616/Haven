use super::{
    LspOperation, LspTool, document_position, operation_specs, redact_lsp_file_uris,
    resolve_lsp_path, select_lsp_server,
};
use crate::Tool;
use haven_common::config::LspServerConfig;
use haven_common::types::{NetworkPolicy, SandboxMode};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

#[test]
fn converts_one_based_utf16_positions_to_lsp_coordinates() {
    let source = "first\nA🦀B\n";
    assert_eq!(document_position(source, 2, 4).unwrap(), (1, 3));
    assert!(document_position(source, 2, 5).is_err());
    assert!(document_position(source, 0, 1).is_err());
}

#[tokio::test]
async fn workspace_write_lsp_contract_declares_network_consent_and_sandboxing() {
    let authorization = Arc::new(crate::security::AuthorizationEngine::new());
    authorization
        .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Deny)
        .await;
    let tool = LspTool::new(Default::default(), authorization);
    let contract = tool
        .authorization_contract(&json!({"operation":"definition", "path":"src/main.rs"}))
        .await
        .expect("LSP authorization policy resolves");

    assert_eq!(contract.policy.network_access, crate::NetworkAccess::Opaque);
    assert_eq!(
        contract.policy.execution_boundary,
        crate::ExecutionBoundary::SandboxedHost
    );
}

#[tokio::test]
async fn full_access_lsp_contract_keeps_host_and_opaque_network_boundary() {
    let authorization = Arc::new(crate::security::AuthorizationEngine::new());
    authorization
        .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
        .await;
    let tool = LspTool::new(Default::default(), authorization);
    let contract = tool
        .authorization_contract(&json!({"operation":"definition", "path":"src/main.rs"}))
        .await
        .expect("LSP authorization policy resolves");

    assert_eq!(contract.policy.network_access, crate::NetworkAccess::Opaque);
    assert_eq!(
        contract.policy.execution_boundary,
        crate::ExecutionBoundary::HostProcess
    );
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
fn workspace_symbol_server_selection_uses_only_enabled_named_servers() {
    let servers = vec![
        LspServerConfig {
            name: "rust-analyzer".into(),
            command: "rust-analyzer".into(),
            extensions: vec![".rs".into()],
            language_id: "rust".into(),
            enabled: true,
            args: Vec::new(),
        },
        LspServerConfig {
            name: "disabled".into(),
            command: "unused".into(),
            extensions: vec![".go".into()],
            language_id: "go".into(),
            enabled: false,
            args: Vec::new(),
        },
    ];

    assert_eq!(
        super::select_lsp_server_by_name(servers.clone(), "RUST-ANALYZER")
            .unwrap()
            .language_id,
        "rust"
    );
    assert!(super::select_lsp_server_by_name(servers.clone(), "disabled").is_none());
    assert!(super::select_lsp_server_by_name(servers, "missing").is_none());
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
fn lsp_specs_declare_sandboxed_offline_policy() {
    for spec in operation_specs() {
        assert_eq!(spec.policy.risk_level, crate::RiskLevel::High);
        assert_eq!(spec.policy.network_access, crate::NetworkAccess::None);
        assert_eq!(
            spec.policy.execution_boundary,
            crate::ExecutionBoundary::SandboxedHost
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

#[test]
fn workspace_symbol_queries_need_no_document_position() {
    let operation = LspOperation::parse("workspace_symbols").unwrap();
    let params = operation
        .params(&json!({"query":"SessionActor"}), None, None)
        .unwrap();

    assert_eq!(operation.method(), "workspace/symbol");
    assert_eq!(params, json!({"query":"SessionActor"}));
}

#[test]
fn implementation_and_type_definition_use_standard_lsp_methods() {
    let uri = "file:///workspace/src/lib.rs";
    let position = Some((2, 5));

    let implementation = LspOperation::parse("implementation").unwrap();
    assert_eq!(implementation.method(), "textDocument/implementation");
    assert_eq!(
        implementation
            .params(&json!({}), Some(uri), position)
            .unwrap(),
        json!({"textDocument":{"uri":uri},"position":{"line":2,"character":5}})
    );

    let type_definition = LspOperation::parse("type_definition").unwrap();
    assert_eq!(type_definition.method(), "textDocument/typeDefinition");
    assert_eq!(
        type_definition
            .params(&json!({}), Some(uri), position)
            .unwrap(),
        json!({"textDocument":{"uri":uri},"position":{"line":2,"character":5}})
    );
}
