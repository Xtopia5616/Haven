use super::*;

#[tokio::test]
async fn content_search_uses_confirmed_read_scope() {
    use crate::{AuthorizationDecision, AuthorizationRequest, DeclaredResource, LocalPathAccess};
    use haven_common::config::ToolConfig;
    use haven_common::types::{NetworkPolicy, SandboxMode};
    let root = TempDir::new().unwrap();
    let allowed = root.path().join("allowed");
    let selected = root.path().join("selected");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::write(selected.join("example.txt"), "needle").unwrap();
    let engine = Arc::new(AuthorizationEngine::new());
    engine.set_boundaries(SandboxMode::WorkspaceWrite, vec![allowed.clone()], NetworkPolicy::Deny).await;
    let mut settings = ToolConfig::default();
    settings.resource_policy.files.path_roots = vec![allowed.to_string_lossy().into_owned()];
    engine.set_tool_settings(std::collections::HashMap::from([("files".into(), settings)])).await;
    let tool = FilesTool::default().with_authorization_engine(Arc::clone(&engine));
    let input = json!({"operation":"search", "root":selected, "pattern":"needle", "mode":"content"});
    let request = AuthorizationRequest::new(None, "files", input.clone(), tool.try_operation_policy(&input).unwrap())
        .with_resources(vec![DeclaredResource::LocalPath { path: selected.clone(), access: LocalPathAccess::Read }]);
    let check = engine.authorize_for_execution(&request).await;
    let AuthorizationDecision::RequiresConfirmation { receipt, .. } = check.decision else { panic!("search needs confirmation") };
    let proof = engine.confirm_for_execution(&request, &receipt).await.unwrap();
    let context = engine.execution_context_for_proof(&request, &proof).await.unwrap();
    let result = tool.run_with_execution_context(serde_json::from_value(input).unwrap(), CancellationToken::new(), &context).await.unwrap();
    assert_eq!(result.output["results"].as_array().unwrap().len(), 1);
}

#[test]
    fn test_sanitize_path_normal() {
        let result = sanitize_path("file.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file.txt");
    }

    #[test]
    fn test_sanitize_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dotted = tmp.path().join("..").join("file.txt");
        let path_str = dotted.to_string_lossy().to_string();
        let result = sanitize_path(&path_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_path_relative() {
        let result = sanitize_path("relative/path/file.txt");
        assert!(result.is_ok());
        assert!(!result.unwrap().contains(".."));
    }

    #[test]
    fn files_retry_safety_separates_reads_from_mutations() {
        let tool = FilesTool::default();
        assert_eq!(
            tool.idempotency(&json!({"operation": "summary"})),
            OperationIdempotency::Idempotent
        );
        assert_eq!(
            tool.idempotency(&json!({"operation": "search"})),
            OperationIdempotency::Idempotent
        );
        assert_eq!(
            tool.idempotency(&json!({"operation": "write"})),
            OperationIdempotency::NonIdempotent
        );
        assert_eq!(
            tool.idempotency(&json!({"operation": "patch"})),
            OperationIdempotency::NonIdempotent
        );
        assert_eq!(tool.idempotency(&json!({})), OperationIdempotency::Unknown);
    }

    #[test]
    fn relative_file_paths_use_workspace_root_when_available() {
        let workspace = crate::execution_workspace::ExecutionWorkspace::discover();
        let root = workspace.root().expect("test checkout has a workspace root");
        let snapshot = workspace.snapshot();
        let resolved = resolve_workspace_path(&snapshot, "docs/architecture.md").unwrap();
        assert_eq!(Path::new(&resolved), root.join("docs/architecture.md"));
    }

    #[test]
    fn relative_file_paths_use_the_injected_execution_workspace() {
        let root = TempDir::new().unwrap();
        let workspace = crate::execution_workspace::ExecutionWorkspace::from_root(root.path())
            .expect("temporary directory is a valid execution root");
        let snapshot = workspace.snapshot();
        let resolved = resolve_workspace_path(&snapshot, "nested/notes.md").unwrap();
        let canonical_root = workspace.root().unwrap();
        assert_eq!(Path::new(&resolved), canonical_root.join("nested/notes.md"));
    }

    #[test]
    fn relative_file_paths_fail_when_the_execution_workspace_is_unavailable() {
        let workspace = crate::execution_workspace::ExecutionWorkspace::unavailable();
        let snapshot = workspace.snapshot();
        assert!(resolve_workspace_path(&snapshot, "nested/notes.md").is_err());
    }

    #[test]
    fn classify_file_by_extension_image() {
        let classification = classify_file_by_extension("photo.PNG");
        assert_eq!(classification.file_kind.as_str(), "image");
        assert_eq!(classification.mime_type, "image/png");
        assert_eq!(classify_file_by_extension("a.jpg").file_kind.as_str(), "image");
        assert_eq!(classify_file_by_extension("a.jpeg").file_kind.as_str(), "image");
    }

    #[test]
    fn classify_file_by_extension_rich_types() {
        assert_eq!(classify_file_by_extension("a.pdf").file_kind.as_str(), "pdf");
        assert_eq!(classify_file_by_extension("a.zip").file_kind.as_str(), "archive");
        assert_eq!(classify_file_by_extension("a.docx").file_kind.as_str(), "office");
        assert_eq!(classify_file_by_extension("a.xlsx").file_kind.as_str(), "office");
        assert_eq!(
            classify_file_by_extension("a.exe").file_kind.as_str(),
            "executable"
        );
        assert_eq!(classify_file_by_extension("no_ext").file_kind.as_str(), "unknown");
        assert_eq!(classify_file_by_extension("a.txt").file_kind.as_str(), "unknown");
    }

    #[test]
    fn classify_file_by_extension_uses_canonical_mime_types() {
        assert_eq!(
            classify_file_by_extension("voice.aac").mime_type,
            "audio/aac"
        );
        assert_eq!(
            classify_file_by_extension("voice.opus").mime_type,
            "audio/opus"
        );
        assert_eq!(
            classify_file_by_extension("clip.mts").mime_type,
            "video/mp2t"
        );
        assert_eq!(
            classify_file_by_extension("photo.heic").mime_type,
            "image/heic"
        );
    }

    #[test]
    fn test_binary_result_carries_file_type() {
        let r = binary_result("report.pdf", 1024);
        let out = &r.output;
        assert_eq!(out["binary"], serde_json::json!(true));
        assert_eq!(out["file_type"], serde_json::json!("pdf"));
        assert!(out["mime"].as_str().unwrap().contains("pdf"));
        assert!(out["hint"].as_str().unwrap().contains("PDF"));
    }
