use super::*;

#[tokio::test]
    async fn test_path_media_read_hands_off_to_media_without_host_path() {
        let tmp = TempDir::new().unwrap();
        let image = tmp.path().join("img.png");
        let audio = tmp.path().join("recording.wav");
        let video = tmp.path().join("clip.mts");
        tokio::fs::write(&image, b"not decoded by files")
            .await
            .unwrap();
        tokio::fs::write(&audio, b"RIFF....WAVE").await.unwrap();
        tokio::fs::write(&video, b"not decoded by files")
            .await
            .unwrap();

        let registry = ManagedAssetRegistry::default();
        let tool = files_tool_with_registry(registry);
        for path in [image, audio, video] {
            let result = tool
                .execute(
                    json!({"operation": "read", "path": path.to_string_lossy()}),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            assert!(result.success);
            assert!(result.output["asset_id"].as_str().is_some());
            assert!(result.output["media"]["asset_id"].as_str().is_some());
            assert!(result.output["media"]["available_representations"].is_array());
            assert!(
                !serde_json::to_string(&result.output)
                    .unwrap()
                    .contains(&tmp.path().to_string_lossy().to_string())
            );
            assert!(result.llm_usage.is_empty());
        }
    }

    struct SummaryUsageMock;

    #[async_trait]
    impl haven_llm::LlmClient for SummaryUsageMock {
        async fn chat(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<haven_llm::LlmResponse, haven_llm::LlmError> {
            Ok(haven_llm::LlmResponse {
                text: "summary".into(),
                usage: haven_llm::Usage {
                    prompt_tokens: 13,
                    completion_tokens: 5,
                    total_tokens: 18,
                    ..Default::default()
                },
                model: Some("small-test".into()),
                ..Default::default()
            })
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<
                            Item = Result<haven_llm::StreamChunk, haven_llm::LlmError>,
                        > + Send,
                >,
            >,
            haven_llm::LlmError,
        > {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<haven_llm::LlmResponse, haven_llm::LlmError> {
            self.chat(messages).await
        }

        async fn health_check(&self) -> Result<(), haven_llm::LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_summary_reports_tool_usage_without_agent_accounting() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("notes.txt");
        tokio::fs::write(&file, "notes for summarization")
            .await
            .unwrap();
        let client = Arc::new(SummaryUsageMock);
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        router
            .force_request_configured(RequestKind::FastChat, true)
            .await;
        let mut tool = FilesTool::default();
        tool.summarizer = Some(router);

        let result = tool
            .execute(
                json!({"operation": "summary", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(result.output["summary"], "summary");
        assert_eq!(result.llm_usage.len(), 1);
        assert_eq!(result.llm_usage[0].call_kind, "tool");
        assert_eq!(result.llm_usage[0].request, RequestKind::FastChat);
        assert_eq!(result.llm_usage[0].usage.total_tokens, 18);
    }
