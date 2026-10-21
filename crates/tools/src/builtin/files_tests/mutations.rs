use super::*;
use crate::security::FileWritePolicy;
use crate::{Tool, ToolExecutionContext};

#[tokio::test]
    async fn test_file_execute_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("output.txt");
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default()
            .execute(
                json!({"operation": "write", "path": path_str, "content": "written content", "expected_hash": "missing"}),
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
    async fn test_file_write_creates_missing_parent_directories() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("new").join("nested").join("output.txt");
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "write",
                    "path": file.to_string_lossy(),
                    "content": "created with parents",
                    "expected_hash": "missing"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "created with parents"
        );
    }

    #[tokio::test]
    async fn test_file_write_dry_run_does_not_create_missing_parents() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("new").join("nested");
        let file = parent.join("output.txt");
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "write",
                    "path": file.to_string_lossy(),
                    "content": "dry run",
                    "expected_hash": "missing",
                    "dry_run": true
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["dry_run"], true);
        assert!(!parent.exists());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_file_write_hash_mismatch_does_not_create_missing_parents() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("new").join("nested");
        let file = parent.join("output.txt");
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "write",
                    "path": file.to_string_lossy(),
                    "content": "guarded write",
                    "expected_hash": "sha256:stale"
                }),
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_err());
        assert!(!parent.exists());
        assert!(!file.exists());
    }

#[tokio::test]
async fn proof_derived_directory_policy_confines_file_writes() {
    let tmp = TempDir::new().unwrap();
    let allowed = tmp.path().join("allowed");
    let outside = tmp.path().join("outside");
    tokio::fs::create_dir(&allowed).await.unwrap();
    tokio::fs::create_dir(&outside).await.unwrap();
    let policy = FileWritePolicy::for_test_roots(std::slice::from_ref(&allowed));
    let context = ToolExecutionContext {
        execution_workspace: None,
        file_write_policy: Some(policy),
        execution_authorization: Some(crate::security::ExecutionPolicyAuthorization::for_test()),
        ..Default::default()
    };
    let tool = FilesTool::default();

    let allowed_file = allowed.join("created.txt");
    let allowed_result = tool
        .execute_with_authorization_context(
            json!({
                "operation": "write",
                "path": allowed_file.to_string_lossy(),
                "content": "inside capability",
                "expected_hash": "missing"
            }),
            CancellationToken::new(),
            None,
            &context,
        )
        .await
        .unwrap();
    assert!(allowed_result.success);
    assert_eq!(
        tokio::fs::read_to_string(&allowed_file).await.unwrap(),
        "inside capability"
    );

    let denied_file = outside.join("denied.txt");
    let denied_result = tool
        .execute_with_authorization_context(
            json!({
                "operation": "write",
                "path": denied_file.to_string_lossy(),
                "content": "outside capability",
                "expected_hash": "missing"
            }),
            CancellationToken::new(),
            None,
            &context,
        )
        .await;
    assert!(denied_result.is_err());
    assert!(!denied_file.exists());
}

#[tokio::test]
async fn file_mutation_rejects_a_policy_change_after_proof_capture() {
    use crate::security::AuthorizationEngine;
    use crate::{
        AuthorizationDecision, AuthorizationRequest, ConfirmationRequirement, DataSensitivity,
        NetworkAccess, OperationEffect, OperationIdempotency, OperationPolicy, ToolConcurrency,
        ToolOperationScope,
    };
    use haven_common::types::{
        CapabilityScope, NetworkPolicy, PermissionMode, SandboxMode,
    };

    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("must-not-write.txt");
    let input = json!({
        "operation": "write",
        "path": file.to_string_lossy(),
        "content": "stale authorization must not write",
        "expected_hash": "missing"
    });
    let policy = OperationPolicy {
        risk_level: RiskLevel::Low,
        capability: CapabilityScope::try_new("files.write").unwrap(),
        confirmation: ConfirmationRequirement::None,
        idempotency: OperationIdempotency::NonIdempotent,
        scope: ToolOperationScope::Session,
        concurrency: ToolConcurrency::Exclusive,
        effect: OperationEffect::WorkspaceWrite,
        data_sensitivity: DataSensitivity::UserData,
        network_access: NetworkAccess::None,
        execution_boundary: crate::ExecutionBoundary::InProcess,
    };
    let request = AuthorizationRequest::new(
        Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "files.write",
        input.clone(),
        policy,
    );
    let engine = Arc::new(AuthorizationEngine::new());
    engine
        .set_boundaries(
            SandboxMode::WorkspaceWrite,
            vec![tmp.path().to_path_buf()],
            NetworkPolicy::Open,
        )
        .await;
    engine.set_permission_mode(PermissionMode::Autonomous).await;
    let check = engine.authorize_for_execution(&request).await;
    assert!(matches!(
        check.decision,
        AuthorizationDecision::AutoApproved
    ));
    let proof = check.execution_proof.unwrap();
    let context = engine
        .execution_context_for_proof(&request, &proof)
        .await
        .unwrap();

    engine
        .set_boundaries(SandboxMode::ReadOnly, Vec::new(), NetworkPolicy::Deny)
        .await;

    let error = FilesTool::default()
        .run_with_execution_context(
            serde_json::from_value(input).unwrap(),
            CancellationToken::new(),
            &context,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("execution policy changed"));
    assert!(!file.exists());
}

#[tokio::test]
async fn file_mutation_requires_a_live_policy_permit_even_with_a_directory_policy() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("must-not-write.txt");
    let context = ToolExecutionContext {
        execution_workspace: None,
        file_write_policy: Some(FileWritePolicy::default()),
        execution_authorization: None,
        ..Default::default()
    };

    let error = FilesTool::default()
        .run_with_execution_context(
            serde_json::from_value(json!({
                "operation": "write",
                "path": file.to_string_lossy(),
                "content": "missing policy permit",
                "expected_hash": "missing"
            }))
            .unwrap(),
            CancellationToken::new(),
            &context,
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("live proof-derived policy permit"));
    assert!(!file.exists());
}

    #[tokio::test]
    async fn test_file_execute_patch_applies_single_exact_replacement() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-single.txt");
        tokio::fs::write(&file, "hello\nworld\nfoo\n")
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({
                    "operation": "patch",
                    "path": path_str,
                    "edits": [{"old_string": "world", "new_string": "there"}]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["edits"].as_u64().unwrap(), 1);
        assert_eq!(result.output["replacements"][0]["lines"], json!([2]));
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "hello\nthere\nfoo\n");
    }

    #[tokio::test]
    async fn test_file_execute_patch_single_replacement_not_found() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-not-found.txt");
        tokio::fs::write(&file, "hello\nworld\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({
                    "operation": "patch",
                    "path": path_str,
                    "edits": [{"old_string": "world\nmissing", "new_string": "x"}]
                }),
                CancellationToken::new(),
            )
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("candidate context"));
        assert!(error.contains("line") || error.contains("2: world"));
    }

    #[tokio::test]
    async fn test_file_execute_patch_single_replacement_rejects_ambiguous_match() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-ambiguous.txt");
        tokio::fs::write(&file, "foo\nfoo\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FilesTool::default().execute(
                json!({
                    "operation": "patch",
                    "path": path_str,
                    "edits": [{"old_string": "foo", "new_string": "bar"}]
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "foo\nfoo\n");
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
    async fn test_file_execute_patch_replaces_all_only_when_requested() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-replace-all.txt");
        tokio::fs::write(&file, "foo\nfoo\nfoo\n").await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "foo", "new_string": "bar", "replace_all": true}]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["replacements"][0]["matches"], 3);
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            "bar\nbar\nbar\n"
        );
    }

    #[tokio::test]
    async fn test_file_execute_patch_bounds_replace_all_match_count() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-too-many-matches.txt");
        let original = vec![b'a'; MAX_PATCH_MATCHES + 1];
        tokio::fs::write(&file, &original).await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "a", "new_string": "b", "replace_all": true}]
                }),
                CancellationToken::new(),
            )
            .await;

        assert!(result.unwrap_err().to_string().contains("100000 matches"));
        assert_eq!(tokio::fs::read(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_applies_single_file_unified_diff_with_shifted_hunks() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-unified.txt");
        let original = (1..=24)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let mut changed = original.clone();
        changed = changed.replace("line 2\n", "LINE TWO\nadded\n");
        changed = changed.replace("line 22\n", "LINE TWENTY TWO\n");
        let diff = diffy::create_patch(&original, &changed).to_string();
        let current = format!("header\n{original}");
        tokio::fs::write(&file, &current).await.unwrap();

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "unified_diff": diff
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["format"], "unified_diff");
        assert!(result.output["hunks"].as_u64().unwrap() >= 2);
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            format!("header\n{changed}")
        );
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_multifile_and_unmatched_unified_diffs() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-unified-reject.txt");
        let original = "alpha\nbeta\ngamma\n";
        tokio::fs::write(&file, original).await.unwrap();

        let multi_file = "--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-alpha\n+ALPHA\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-beta\n+BETA\n";
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "unified_diff": multi_file
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("exactly one file patch"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);

        let mismatched = "--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n alpha\n-BETA\n+BETTER\n gamma\n";
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "unified_diff": mismatched
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("did not match"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_rejects_unified_file_creation_and_patch_file_over_limit() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-bounds.txt");
        let original = "alpha\nbeta\n";
        tokio::fs::write(&file, original).await.unwrap();

        let creation = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+new\n";
        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "unified_diff": creation
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("creation"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);

        let limited = FilesTool {
            max_patch_file_bytes: 4,
            ..FilesTool::default()
        };
        let result = limited
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "alpha", "new_string": "ALPHA"}]
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("patch limit"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), original);
    }

    #[tokio::test]
    async fn test_file_execute_patch_guards_the_version_it_read() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("patch-hash.txt");
        tokio::fs::write(&file, "before\n").await.unwrap();
        let stale_hash = "sha256:stale";

        let result = FilesTool::default()
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "before", "new_string": "after"}],
                    "expected_hash": stale_hash
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("expected_hash"));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "before\n");
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

        let large_original = format!("{}tail\n", "x".repeat(128_001));
        tokio::fs::write(&file, &large_original).await.unwrap();
        let tool = FilesTool::default();
        let result = tool
            .execute(
                json!({
                    "operation": "patch",
                    "path": file.to_string_lossy(),
                    "edits": [{"old_string": "tail", "new_string": "done"}]
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(
            tokio::fs::read_to_string(&file).await.unwrap(),
            format!("{}done\n", "x".repeat(128_001))
        );
    }

    #[tokio::test]
    async fn configured_file_read_caps_do_not_set_the_write_cap() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("independent-write-cap.txt");
        let path = file.to_string_lossy().to_string();
        let tool = FilesTool {
            max_full_file_bytes: 1,
            max_byte_read_bytes: 1,
            ..FilesTool::default()
        };

        let result = tool
            .execute(
                json!({"operation": "write", "path": path, "content": "ok", "expected_hash": "missing"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "ok");

        let limited_file = tmp.path().join("explicit-write-cap.txt");
        let limited = FilesTool::default().with_max_write_bytes(1);
        let result = limited
            .execute(
                json!({
                    "operation": "write",
                    "path": limited_file.to_string_lossy(),
                    "content": "ok",
                    "expected_hash": "missing"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(!limited_file.exists());

        let patch_limited_file = tmp.path().join("explicit-patch-write-cap.txt");
        tokio::fs::write(&patch_limited_file, "a").await.unwrap();
        let limited = FilesTool::default().with_max_write_bytes(1);
        let result = limited
            .execute(
                json!({
                    "operation": "patch",
                    "path": patch_limited_file.to_string_lossy(),
                    "edits": [{"old_string": "a", "new_string": "ok"}]
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(tokio::fs::read_to_string(&patch_limited_file).await.unwrap(), "a");
    }

    #[test]
    fn patch_preserves_supported_text_encoding_when_writing() {
        let original = "你好\n";
        let edit = FilesPatchEdit {
            old_string: "你好".into(),
            new_string: "世界".into(),
            expected_matches: None,
            replace_all: false,
        };
        for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be", "gbk"] {
            let encoded = encode_patched_text(original, encoding).unwrap();
            let decoded = haven_common::encoding::decode_with_encoding(&encoded);
            let patch = apply_patch_edits(
                &decoded.text,
                std::slice::from_ref(&edit),
                1024,
                CancellationToken::new(),
            )
            .unwrap();
            let written = encode_patched_text(&patch.content, decoded.encoding).unwrap();
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
    async fn test_file_delete_removes_empty_directory_but_not_nonempty_directory() {
        let tmp = TempDir::new().unwrap();
        let empty = tmp.path().join("empty");
        tokio::fs::create_dir(&empty).await.unwrap();
        let result = FilesTool::default()
            .execute(
                json!({"operation": "delete", "path": empty.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["file_type"], "directory");
        assert!(!empty.exists());

        let nonempty = tmp.path().join("nonempty");
        tokio::fs::create_dir(&nonempty).await.unwrap();
        let child = nonempty.join("keep.txt");
        tokio::fs::write(&child, "keep").await.unwrap();
        let result = FilesTool::default()
            .execute(
                json!({"operation": "delete", "path": nonempty.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(child.exists());
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

        let tool = FilesTool {
            max_list_entries: 1,
            ..FilesTool::default()
        };
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
