use super::*;

#[test]
    fn test_looks_like_binary() {
        assert!(!looks_like_binary(b"hello world\nplain text"));
        assert!(looks_like_binary(b"\x00\x01\x02"));
        assert!(looks_like_binary(b"text with \x00 nul inside"));
    }

    #[tokio::test]
    async fn test_full_read_cursor_points_at_first_omitted_character() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("bounded.txt");
        tokio::fs::write(&file, "abcdefghij").await.unwrap();
        let mut tool = FilesTool::default();
        tool.max_output_chars = 4;

        let result = tool
            .execute(
                json!({"operation": "read", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.truncated);
        assert_eq!(result.output["next_offset"], 4);
    }

    /// Write `content` to `<tmp>/<name>` and run a `read` operation on it,
    /// returning the tool result. Shared by the too-large read tests.
    async fn write_and_read(tmp: &TempDir, name: &str, content: impl AsRef<[u8]>) -> ToolResult {
        let file = tmp.path().join(name);
        tokio::fs::write(&file, content).await.unwrap();
        let path_str = file.to_string_lossy().to_string();
        FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str}),
                CancellationToken::new(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_file_execute_read_too_large() {
        let tmp = TempDir::new().unwrap();
        let result = write_and_read(&tmp, "big.txt", vec![b'a'; (128_000 + 1) as usize]).await;
        assert!(result.success);
        assert!(result.output["too_large"].as_bool().unwrap());
        assert!(
            result.output["hint"]
                .as_str()
                .unwrap()
                .contains("offset/limit")
        );
        assert!(result.output["next_offset"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_file_execute_read_too_large_utf8_head_decodes_cleanly() {
        // The head read (max_chars * 4 bytes) ends mid-CJK-sequence; the
        // returned head must still decode as UTF-8, not as GBK mojibake.
        let tmp = TempDir::new().unwrap();
        let content = "中".repeat(128_000 / 3 + 100);
        let result = write_and_read(&tmp, "big_cjk.txt", &content).await;
        assert!(result.success);
        assert!(result.output["too_large"].as_bool().unwrap());
        let head = result.output["content"].as_str().unwrap();
        assert!(
            head.starts_with("中中"),
            "head must keep UTF-8 content, got: {}",
            &head[..head.len().min(40)]
        );
        assert!(
            !head.contains('\u{FFFD}'),
            "head must not contain replacement chars"
        );
        assert!(result.output["truncated"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_file_execute_read_too_large_gbk_head_still_decodes() {
        // GBK-encoded content must still fall back to GBK decoding for the head.
        let tmp = TempDir::new().unwrap();
        let gbk_line = [0xC4, 0xE3, 0xBA, 0xC3]; // "你好" in GBK
        let mut content = Vec::with_capacity(128_000 + 4);
        while content.len() <= 128_000 {
            content.extend_from_slice(&gbk_line);
        }
        let result = write_and_read(&tmp, "big_gbk.txt", &content).await;
        assert!(result.success);
        assert!(result.output["too_large"].as_bool().unwrap());
        let head = result.output["content"].as_str().unwrap();
        assert!(
            head.contains("你好"),
            "GBK head must decode to CJK text, got: {}",
            &head[..head.len().min(40)]
        );
        assert_eq!(result.output["encoding"], "gbk");
    }

    #[tokio::test]
    async fn test_file_execute_read_binary() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("blob.bin");
        tokio::fs::write(&file, b"\x00\x01\x02\x03binary\x00")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["binary"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_file_execute_read_bytes_mode() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("data.txt");
        tokio::fs::write(&file, "0123456789abcdefghij")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str, "offset": 5, "limit": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "56789");
        assert_eq!(result.output["mode"].as_str().unwrap(), "bytes");
        assert_eq!(result.output["offset"].as_u64().unwrap(), 5);
        assert_eq!(result.output["total_bytes"].as_u64().unwrap(), 20);
        assert_eq!(result.output["encoding"], "utf-8");
        assert!(result.output["truncated"].as_bool().unwrap());
        assert_eq!(result.output["next_offset"].as_u64().unwrap(), 10);
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_mode() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        tokio::fs::write(&file, "line1\nline2\nline3\nline4\nline5\n")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str, "start_line": 2, "end_line": 4}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["mode"].as_str().unwrap(), "lines");
        assert_eq!(result.output["start_line"].as_u64().unwrap(), 2);
        assert_eq!(result.output["end_line"].as_u64().unwrap(), 4);
        assert_eq!(
            result.output["content"].as_str().unwrap(),
            "line2\nline3\nline4\n"
        );
        assert_eq!(result.output["encoding"], "utf-8");
    }

    #[tokio::test]
    async fn test_file_execute_outline_returns_bounded_symbols_and_line_numbers() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("sample.rs");
        tokio::fs::write(
            &file,
            "# heading\n\npub struct User {\n}\n\nimpl User {\n    pub fn name(&self) {}\n}\n",
        )
        .await
        .unwrap();
        let result = FilesTool::default()
            .execute(
                json!({"operation": "outline", "path": file.to_string_lossy(), "max_symbols": 2}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.truncated);
        assert_eq!(result.output["count"], 2);
        assert_eq!(result.output["symbols"][0]["line"], 1);
        assert_eq!(result.output["symbols"][0]["kind"], "heading");
        assert_eq!(result.output["symbols"][1]["name"], "User");
        assert_eq!(result.output["next_start_line"], 6);

        let continuation = FilesTool::default()
            .execute(
                json!({
                    "operation": "outline",
                    "path": file.to_string_lossy(),
                    "start_line": 6,
                    "max_symbols": 2
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(continuation.success);
        assert_eq!(continuation.output["symbols"][0]["line"], 6);
        assert_eq!(continuation.output["symbols"][1]["line"], 7);
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_last_chunk_not_truncated() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        tokio::fs::write(&file, "line1\nline2\nline3\n")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str, "start_line": 3}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "line3\n");
        assert!(!result.output["truncated"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_first_line_over_budget_flags_truncated() {
        // A single 50KB line exceeds the output budget on the first candidate
        // line: the empty result must still report truncated, not "empty range".
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("huge_line.txt");
        tokio::fs::write(&file, format!("{}\n", "x".repeat(50_000)))
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str, "start_line": 1}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "");
        assert!(result.output["truncated"].as_bool().unwrap());
        assert_eq!(result.output["next_start_line"], 1);
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_gbk_budget_flags_truncated() {
        // GBK decode expands bytes (2 raw bytes -> 3 UTF-8 bytes). The budget
        // check must compare decoded lengths so the returned content stays
        // within max_chars even for non-UTF-8 files.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("gbk_lines.txt");
        let gbk_line = [0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7, b'\n']; // "你好世界\n"
        let mut content = Vec::new();
        while content.len() < 15_000 {
            content.extend_from_slice(&gbk_line);
        }
        tokio::fs::write(&file, &content).await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str, "start_line": 1}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let out = result.output["content"].as_str().unwrap();
        assert!(
            out.len() <= 20_000,
            "content exceeds budget: {} bytes",
            out.len()
        );
        assert!(result.output["truncated"].as_bool().unwrap());
        assert!(
            out.contains("你好"),
            "GBK content must decode, got: {}",
            &out[..out.len().min(30)]
        );
    }

    #[tokio::test]
    async fn test_file_execute_read() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("readme.txt");
        tokio::fs::write(&file, "hello world").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "read", "path": path_str}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(!result.truncated);
        assert_eq!(result.output["content"].as_str().unwrap(), "hello world");
        assert_eq!(result.output["operation"], "read");
        assert_eq!(result.output["path"], path_str);
        assert_eq!(result.output["truncated"], false);
    }
