use super::*;

#[tokio::test]
    async fn test_file_execute_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("output.txt");
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "write", "path": path_str, "content": "written content"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["written"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "written content");
    }

    #[tokio::test]
    async fn test_file_execute_edit() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "hello\nworld\nfoo\n")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "world", "new_string": "there"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["line"].as_u64().unwrap(), 2);
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "hello\nthere\nfoo\n");
    }

    #[tokio::test]
    async fn test_file_execute_edit_not_found() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "hello\nworld\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "nope", "new_string": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_execute_edit_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "foo\nfoo\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "foo", "new_string": "bar"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output["warning"]
                .as_str()
                .unwrap()
                .contains("2 times")
        );
        assert_eq!(result.output["matches"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_file_execute_patch_applies_multiple_exact_replacements_once() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch.txt");
        tokio::fs::write(&file, "alpha\nbeta\ngamma\n")
            .await
            .unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "gamma", "new_string": "GAMMA"}
                    ]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["patched"], true);
        assert_eq!(result.output["edits"], 2);
        assert_eq!(result.output["replacements"][0]["matches"], 1);
        assert_eq!(result.output["replacements"][0]["lines"], json!([1]));
        assert_eq!(result.output["replacements"][1]["lines"], json!([3]));
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "ALPHA\nbeta\nGAMMA\n"
        );
    }

    #[tokio::test]
    async fn test_file_execute_patch_allows_explicit_repeated_match_count() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-repeated.txt");
        tokio::fs::write(&file, "foo\nfoo\n").await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{
                        "old_string": "foo",
                        "new_string": "bar",
                        "expected_matches": 2
                    }]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["replacements"][0]["matches"], 2);
        assert_eq!(result.output["replacements"][0]["lines"], json!([1, 2]));
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "bar\nbar\n"
        );
    }

    #[tokio::test]
    async fn test_file_execute_patch_is_transactional_when_one_edit_fails() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-transaction.txt");
        let original = "alpha\nbeta\n";
        tokio::fs::write(&file, original).await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [
                        {"old_string": "alpha", "new_string": "ALPHA"},
                        {"old_string": "missing", "new_string": "MISSING"}
                    ]
                }),
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_repeated_or_overlapping_targets() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-overlap.txt");
        let original = "abcdef\n";
        tokio::fs::write(&file, original).await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [
                        {"old_string": "abcdef", "new_string": "whole"},
                        {"old_string": "cde", "new_string": "middle"}
                    ]
                }),
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_empty_input_and_empty_old_string() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-empty.txt");
        let original = "content\n";
        tokio::fs::write(&file, original).await.unwrap();
        let path = file.to_string_lossy().to_string();

        let empty = FilesTool::default()
            .execute(
                json!({"operation": "patch", "path": path, "edits": []}),
                CancellationToken::new(),
            )
            .await;
        assert!(empty.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);

        let empty_old = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "", "new_string": "x"}]
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(empty_old.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_limits_without_writing() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-limits.txt");
        let original = "abc";
        tokio::fs::write(&file, original).await.unwrap();
        let path = file.to_string_lossy().to_string();

        let too_many = (0..=MAX_PATCH_EDITS)
            .map(|index| json!({"old_string": format!("old-{index}"), "new_string": "x"}))
            .collect::<Vec<_>>();
        let result = FilesTool::default()
            .execute(
                json!({"operation": "patch", "path": path, "edits": too_many}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);

        let mut tool = FilesTool::default();
        tool.max_read_chars = 10;
        let result = tool
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "a", "new_string": "01234567890"}]
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[test]
    fn patch_preserves_supported_text_encoding_when_writing() {
        let original = "你好\n";
        let edit = FilesPatchEdit {
            old_string: "你好".into(),
            new_string: "世界".into(),
            expected_matches: None,
        };
        for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be", "gbk"] {
            let encoded = encode_patched_text(original, encoding).unwrap();
            let decoded = haven_common::encoding::decode_with_encoding(&encoded);
            let (patched, _) = apply_patch_edits(
                &decoded.text,
                std::slice::from_ref(&edit),
                1024,
                CancellationToken::new(),
            )
            .unwrap();
            let written = encode_patched_text(&patched, decoded.encoding).unwrap();
            let roundtrip = haven_common::encoding::decode_with_encoding(&written);
            assert_eq!(roundtrip.text, "世界\n", "encoding={encoding}");
        }
    }

    #[tokio::test]
    async fn test_file_execute_patch_cancelled_before_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-cancelled.txt");
        let original = "alpha\n";
        tokio::fs::write(&file, original).await.unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "alpha", "new_string": "ALPHA"}]
                }),
                cancel,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_unsupported_binary_without_writing() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-binary.bin");
        let original = b"text\0binary";
        tokio::fs::write(&file, original).await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "text", "new_string": "TEXT"}]
                }),
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_copy() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("dest.txt");
        tokio::fs::write(&src, "copy me").await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "copy",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(src.exists());
        assert!(dst.exists());
        let content = tokio::fs::read_to_string(&dst).await.unwrap();
        assert_eq!(content, "copy me");
    }

    #[tokio::test]
    async fn test_file_execute_move() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("target.txt");
        tokio::fs::write(&src, "move me").await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "move",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(!src.exists());
        assert!(dst.exists());
    }

    #[tokio::test]
    async fn test_file_execute_delete() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("to_delete.txt");
        tokio::fs::write(&file, "delete me").await.unwrap();
        assert!(file.exists());

        let result = FilesTool::default()
            .execute(
                json!({"operation": "delete", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["deleted"].as_bool().unwrap());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_file_execute_list() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "a")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("b.txt"), "b")
            .await
            .unwrap();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "list", "path": tmp.path().to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let entries = result.output["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
        assert!(!result.truncated);
        assert_eq!(result.output["operation"], "list");
        assert_eq!(
            result.output["path"],
            tmp.path().to_string_lossy().to_string()
        );
        assert_eq!(result.output["truncated"], false);
    }

    #[tokio::test]
    async fn test_file_execute_list_cap_marks_result_truncated() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "a")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("b.txt"), "b")
            .await
            .unwrap();

        let mut tool = FilesTool::default();
        tool.max_list_entries = 1;
        let result = tool
            .execute(
                json!({"operation": "list", "path": tmp.path().to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.truncated);
        assert_eq!(result.output["truncated"], true);
        assert_eq!(result.output["entries"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_file_execute_create_dir() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("one").join("two");
        let result = FilesTool::default()
            .execute(
                json!({"operation": "create_dir", "path": nested.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["created"], true);
        assert!(nested.is_dir());
    }

    #[tokio::test]
    async fn test_file_execute_unknown() {
        let result = FilesTool::default()
            .execute(
                json!({"operation": "unknown", "path": "file.txt"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = FilesTool::default()
            .execute(json!({"operation": "read", "path": "file.txt"}), cancel)
            .await;
        assert!(result.is_err());
    }
