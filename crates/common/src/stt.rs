use anyhow::Result;

/// Trait for speech-to-text conversion.
/// Implementations receive WAV bytes and return transcribed text.
#[async_trait::async_trait]
pub trait SttClient: Send + Sync {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSttClient {
        response: String,
    }

    #[async_trait::async_trait]
    impl SttClient for MockSttClient {
        async fn transcribe(&self, _wav_data: &[u8]) -> Result<String> {
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_mock_stt_returns_expected_text() {
        let client = MockSttClient {
            response: "hello world".into(),
        };
        let result = client.transcribe(b"fake wav data").await.unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn test_mock_stt_accepts_empty_data() {
        let client = MockSttClient {
            response: String::new(),
        };
        let result = client.transcribe(&[]).await.unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn test_stt_trait_object() {
        let client: Box<dyn SttClient> = Box::new(MockSttClient {
            response: "test".into(),
        });
        let result = client.transcribe(b"data").await.unwrap();
        assert_eq!(result, "test");
    }

    #[tokio::test]
    async fn test_mock_stt_rejects_on_flag() {
        struct ErrClient;

        #[async_trait::async_trait]
        impl SttClient for ErrClient {
            async fn transcribe(&self, _: &[u8]) -> Result<String> {
                anyhow::bail!("transcription failed")
            }
        }

        let client = ErrClient;
        let result = client.transcribe(b"data").await;
        assert!(result.is_err());
    }
}
