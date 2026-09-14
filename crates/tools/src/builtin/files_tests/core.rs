use super::*;

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
        let resolved = resolve_workspace_path("docs/architecture.md").unwrap();
        let current = std::env::current_dir().unwrap();
        let root = haven_common::discover_workspace_root(&current).unwrap();
        assert_eq!(Path::new(&resolved), root.join("docs/architecture.md"));
    }

    #[test]
    fn test_classify_by_extension_image() {
        let (kind, mime) = classify_by_extension("photo.PNG");
        assert_eq!(kind, "image");
        assert_eq!(mime, "image/png");
        let (kind, _) = classify_by_extension("a.jpg");
        assert_eq!(kind, "image");
        let (kind, _) = classify_by_extension("a.jpeg");
        assert_eq!(kind, "image");
    }

    #[test]
    fn test_classify_by_extension_rich_types() {
        assert_eq!(classify_by_extension("a.pdf").0, "pdf");
        assert_eq!(classify_by_extension("a.zip").0, "archive");
        assert_eq!(classify_by_extension("a.docx").0, "office");
        assert_eq!(classify_by_extension("a.xlsx").0, "office");
        assert_eq!(classify_by_extension("a.exe").0, "executable");
        assert_eq!(classify_by_extension("no_ext").0, "unknown");
        assert_eq!(classify_by_extension("a.txt").0, "unknown");
    }

    #[test]
    fn test_classify_by_extension_uses_canonical_media_mimes() {
        assert_eq!(classify_by_extension("voice.aac"), ("audio", "audio/aac"));
        assert_eq!(classify_by_extension("voice.opus"), ("audio", "audio/opus"));
        assert_eq!(classify_by_extension("clip.mts"), ("video", "video/mp2t"));
        assert_eq!(classify_by_extension("photo.heic"), ("image", "image/heic"));
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
