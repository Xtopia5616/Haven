use super::*;

#[tokio::test]
    async fn test_managed_asset_read_uses_id_and_redacts_host_path() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("report.txt");
        tokio::fs::write(&file, "managed content").await.unwrap();
        let path_str = file.to_string_lossy().to_string();
        let registry = ManagedAssetRegistry::default();
        registry.register_for_test("asset-test", file, Some("report.txt".into()), "text/plain");
        let tool = files_tool_with_registry(registry);

        let result = tool
            .execute(
                json!({"operation": "read", "asset_id": "asset-test"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["content"], "managed content");
        assert_eq!(result.output["asset_id"], "asset-test");
        assert_eq!(result.output["filename"], "report.txt");
        assert!(result.output.get("path").is_none());
        assert!(
            !serde_json::to_string(&result.output)
                .unwrap()
                .contains(&path_str)
        );

        let mutation = tool
            .execute(
                json!({"operation": "write", "asset_id": "asset-test", "content": "nope"}),
                CancellationToken::new(),
            )
            .await;
        assert!(mutation.is_err(), "managed assets are read-only");
    }

    #[tokio::test]
    async fn test_managed_asset_revalidates_before_read() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("report.txt");
        tokio::fs::write(&file, "managed content").await.unwrap();
        let registry = ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            tmp.path(),
            "asset-race",
            file.clone(),
            Some("report.txt".into()),
            "text/plain",
        ));
        tokio::fs::remove_file(&file).await.unwrap();
        let tool = files_tool_with_registry(registry);

        let result = tool
            .execute(
                json!({"operation": "read", "asset_id": "asset-race"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("managed asset changed")
        );
    }

    #[tokio::test]
    async fn test_managed_pdf_read_uses_media_extract_and_redacts_host_path() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("report.pdf");
        let body = b"BT\n(Quarterly report) Tj\nET\n";
        let pdf = format!("%PDF-1.4\n1 0 obj\n<< /Length {} >>\nstream\n", body.len());
        let mut bytes = pdf.into_bytes();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"endstream\nendobj\n");
        tokio::fs::write(&file, bytes).await.unwrap();
        let path_str = file.to_string_lossy().to_string();
        let registry = ManagedAssetRegistry::default();
        registry.register_for_test(
            "asset-pdf",
            file,
            Some("report.pdf".into()),
            "application/pdf",
        );
        let tool = files_tool_with_registry(registry);

        let result = tool
            .execute(
                json!({"operation": "read", "asset_id": "asset-pdf"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let content = result.output["media"]["content"].as_str().unwrap();
        assert!(content.contains("Quarterly report"));
        assert!(content.contains("Quarterly report"));
        assert_eq!(result.output["representation"], "document_pages");
        assert_eq!(result.output["untrusted_content"], true);
        assert!(
            !serde_json::to_string(&result.output)
                .unwrap()
                .contains(&path_str)
        );
    }

    #[tokio::test]
    async fn test_supported_document_parse_failure_is_not_reported_as_success() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("broken.pdf");
        tokio::fs::write(&file, b"%PDF-1.7\nnot a readable document")
            .await
            .unwrap();

        let result = files_tool_with_registry(ManagedAssetRegistry::default())
            .execute(
                json!({"operation": "read", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output["asset_id"].as_str().is_some());
        assert!(result.output["media"]["asset_id"].as_str().is_some());
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_unsupported_document_format_is_distinct_from_parse_failure() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("legacy.doc");
        tokio::fs::write(&file, b"legacy binary format")
            .await
            .unwrap();

        let result = files_tool_with_registry(ManagedAssetRegistry::default())
            .execute(
                json!({"operation": "read", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output["asset_id"].as_str().is_some());
        assert!(result.output["media"]["asset_id"].as_str().is_some());
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_summary_rich_path_hands_off_to_media() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("report.pdf");
        let body = b"BT\n(Extracted summary source) Tj\nET\n";
        let pdf = format!("%PDF-1.4\n1 0 obj\n<< /Length {} >>\nstream\n", body.len());
        let mut bytes = pdf.into_bytes();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"endstream\nendobj\n");
        tokio::fs::write(&file, bytes).await.unwrap();

        let result = files_tool_with_registry(ManagedAssetRegistry::default())
            .execute(
                json!({"operation": "summary", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["operation"], "summary");
        assert!(result.output["asset_id"].as_str().is_some());
        assert_eq!(result.output["media"]["representation"], "document_pages");
        assert!(
            result.output["media"]["content"]
                .as_str()
                .unwrap()
                .contains("Extracted summary source")
        );
    }

    #[tokio::test]
    async fn test_summary_budget_counts_decoded_non_utf8_characters() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("legacy.txt");
        // GBK for "你好\n". The decoded text is three characters although the
        // source line occupies five bytes.
        tokio::fs::write(&file, [0xC4, 0xE3, 0xBA, 0xC3, b'\n'])
            .await
            .unwrap();

        let (content, _, _, _, truncated) =
            read_for_summary(&file.to_string_lossy(), 1, 0, 3, 128_000)
                .await
                .unwrap();
        assert_eq!(content, "你好\n");
        assert!(!truncated);
        assert_eq!(content.chars().count(), 3);
    }

    #[tokio::test]
    async fn test_file_native_entry_lands_in_run() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("native.txt");
        let path_str = file.to_string_lossy().to_string();
        let result = FilesTool::default()
            .run(
                FilesParams {
                    operation: Some(FilesOperation::Write),
                    path: Some(path_str.clone()),
                    asset_id: None,
                    destination: None,
                    content: Some("native content".into()),
                    expected_hash: None,
                    dry_run: None,
                    old_string: None,
                    new_string: None,
                    edits: None,
                    offset: None,
                    limit: None,
                    start_line: None,
                    end_line: None,
                    focus: None,
                    max_chars: None,
                    root: None,
                    pattern: None,
                    mode: None,
                    max_depth: None,
                    max_results: None,
                    ignore_hidden: None,
                    max_file_size: None,
                    max_symbols: None,
                    session_id: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["written"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "native content");
    }

    #[tokio::test]
    async fn atomic_write_supports_dry_run_and_compare_and_swap() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("atomic.txt");
        tokio::fs::write(&file, b"before").await.unwrap();
        let path = file.to_string_lossy().to_string();
        let expected = sha256_bytes(b"before");
        let tool = FilesTool::default();

        let dry_run = tool
            .execute(
                json!({
                    "operation": "write",
                    "path": path,
                    "content": "after",
                    "expected_hash": expected,
                    "dry_run": true
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(dry_run.success);
        assert_eq!(dry_run.output["dry_run"], true);
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "before");

        let mismatch = tool
            .execute(
                json!({
                    "operation": "write",
                    "path": file.to_string_lossy(),
                    "content": "after",
                    "expected_hash": sha256_bytes(b"stale")
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(mismatch.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "before");

        let written = tool
            .execute(
                json!({
                    "operation": "write",
                    "path": file.to_string_lossy(),
                    "content": "after",
                    "expected_hash": expected
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(written.output["written"], true);
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "after");
    }

    #[tokio::test]
    async fn inspect_returns_bounded_file_metadata_and_hash() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("inspect.txt");
        tokio::fs::write(&file, "hello").await.unwrap();
        let result = FilesTool::default()
            .execute(
                json!({"operation": "inspect", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["exists"], true);
        assert_eq!(result.output["file_type"], "file");
        assert_eq!(result.output["size"], 5);
        assert_eq!(result.output["hash"], sha256_bytes(b"hello"));
        assert_eq!(result.output["encoding"], "utf-8");
    }
