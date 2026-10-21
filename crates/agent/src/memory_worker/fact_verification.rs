use std::collections::{HashMap, HashSet};

use haven_common::prompts::FACT_VERIFICATION_SYSTEM_PROMPT;
use haven_common::types::is_canonical_id;
use haven_memory::{
    FactEvidenceStatus, MemoryFactProposalWrite, MemoryFactWrite, VERIFIED_FACT_CONFIDENCE_FLOOR,
    is_sensitive_text,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::fact_extraction::extract_json_array;
use crate::memory_inference::MemoryInferencePort;

const MAX_VERIFICATION_BATCH: usize = 32;
const MAX_USER_EVIDENCE_CHARS: usize = 2400;
const MAX_ASSISTANT_CONTEXT_CHARS: usize = 600;
const MAX_EVIDENCE_QUOTE_CHARS: usize = 120;

pub(crate) struct FactEvidenceSource {
    pub(crate) message_id: String,
    pub(crate) user_text: String,
    pub(crate) assistant_context: Option<String>,
}

pub(crate) struct PreparedFactWrite {
    pub(crate) write: MemoryFactWrite,
    pub(crate) evidence: Option<FactEvidenceSource>,
}

#[derive(Serialize)]
struct VerificationCandidate<'a> {
    candidate_index: usize,
    subject: &'a str,
    predicate: &'a str,
    object: &'a str,
    user_message: String,
    assistant_context: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VerificationVerdict {
    Supported,
    Uncertain,
    Contradicted,
}

#[derive(Debug, Deserialize)]
struct VerificationResponse {
    candidate_index: usize,
    verdict: VerificationVerdict,
    #[serde(default)]
    evidence_quote: Option<String>,
}

/// Check whether each extracted fact is directly supported by its original user message.
/// Any missing, malformed, sensitive, or unavailable evidence fails closed to manual review.
pub(crate) async fn verify_fact_writes(
    inference: &dyn MemoryInferencePort,
    semaphore: &Semaphore,
    prepared: Vec<PreparedFactWrite>,
) -> Vec<MemoryFactProposalWrite> {
    let mut candidates = Vec::new();
    for (candidate_index, item) in prepared.iter().enumerate() {
        let Some(evidence) = item.evidence.as_ref() else {
            continue;
        };
        if !is_canonical_id(&evidence.message_id, "msg")
            || evidence.user_text.trim().is_empty()
            || is_sensitive_text(&evidence.user_text)
        {
            continue;
        }
        candidates.push(VerificationCandidate {
            candidate_index,
            subject: &item.write.subject,
            predicate: &item.write.predicate,
            object: &item.write.object,
            user_message: take_chars(&evidence.user_text, MAX_USER_EVIDENCE_CHARS),
            assistant_context: evidence
                .assistant_context
                .as_deref()
                .map(|context| take_chars(context, MAX_ASSISTANT_CONTEXT_CHARS)),
        });
        if candidates.len() >= MAX_VERIFICATION_BATCH {
            break;
        }
    }

    let mut assessments = HashMap::new();
    if !candidates.is_empty() {
        match semaphore.acquire().await {
            Ok(_permit) => match serde_json::to_string(&candidates) {
                Ok(prompt) => match inference
                    .fast_chat(FACT_VERIFICATION_SYSTEM_PROMPT, &prompt)
                    .await
                {
                    Ok(response) => match serde_json::from_str::<Vec<VerificationResponse>>(
                        &extract_json_array(&response),
                    ) {
                        Ok(results) => {
                            assessments = unique_assessments(results);
                        }
                        Err(error) => tracing::warn!(
                            error = %error,
                            "fact evidence verifier returned an invalid response; keeping proposals for review"
                        ),
                    },
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "fact evidence verification failed; keeping proposals for review"
                    ),
                },
                Err(error) => tracing::warn!(
                    error = %error,
                    "fact evidence verification input could not be encoded"
                ),
            },
            Err(error) => tracing::warn!(
                error = %error,
                "fact evidence verifier unavailable; keeping proposals for review"
            ),
        }
    }

    prepared
        .into_iter()
        .enumerate()
        .map(|(candidate_index, mut item)| {
            let evidence = item.evidence.take();
            let mut status = FactEvidenceStatus::Unverified;
            let mut quote = None;
            if let (Some(evidence), Some(result)) =
                (evidence.as_ref(), assessments.get(&candidate_index))
            {
                match result.verdict {
                    VerificationVerdict::Supported => {
                        if let Some(candidate_quote) = result.evidence_quote.as_deref()
                            && !candidate_quote.trim().is_empty()
                            && candidate_quote.trim() == candidate_quote
                            && candidate_quote.chars().count() <= MAX_EVIDENCE_QUOTE_CHARS
                            && evidence.user_text.contains(candidate_quote)
                            && !is_sensitive_text(candidate_quote)
                            && item
                                .write
                                .source_ref
                                .as_ref()
                                .is_some_and(|source| source.message_id == evidence.message_id)
                        {
                            status = FactEvidenceStatus::Supported;
                            quote = Some(candidate_quote.to_owned());
                            if let Some(source) = item.write.source_ref.as_mut() {
                                source.snippet = candidate_quote.to_owned();
                            }
                        }
                    }
                    VerificationVerdict::Uncertain => status = FactEvidenceStatus::Uncertain,
                    VerificationVerdict::Contradicted => status = FactEvidenceStatus::Contradicted,
                }
            }
            if status == FactEvidenceStatus::Supported {
                item.write.confidence = item.write.confidence.max(VERIFIED_FACT_CONFIDENCE_FLOOR);
            }
            MemoryFactProposalWrite {
                write: item.write,
                evidence_status: status,
                evidence_quote: quote,
            }
        })
        .collect()
}

fn unique_assessments(results: Vec<VerificationResponse>) -> HashMap<usize, VerificationResponse> {
    let mut assessments = HashMap::new();
    let mut duplicates = HashSet::new();
    for result in results {
        let candidate_index = result.candidate_index;
        if assessments.insert(candidate_index, result).is_some() {
            assessments.remove(&candidate_index);
            duplicates.insert(candidate_index);
        }
    }
    assessments.retain(|candidate_index, _| !duplicates.contains(candidate_index));
    assessments
}

fn take_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) fn unverified_fact_write(write: MemoryFactWrite) -> MemoryFactProposalWrite {
    MemoryFactProposalWrite {
        write,
        evidence_status: FactEvidenceStatus::Unverified,
        evidence_quote: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{FactEvidenceSource, PreparedFactWrite, verify_fact_writes};
    use crate::memory_inference::MemoryInferencePort;
    use async_trait::async_trait;
    use haven_common::types::new_id;
    use haven_memory::{
        FactEvidenceStatus, FactSourceRef, MemoryFactWrite, VERIFIED_FACT_CONFIDENCE_FLOOR,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    struct StubInference {
        response: String,
        calls: AtomicUsize,
    }

    impl StubInference {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_owned(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl MemoryInferencePort for StubInference {
        async fn is_fast_chat_configured(&self) -> bool {
            true
        }

        async fn fast_chat(&self, _: &str, _: &str) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    fn candidate(user_text: Option<&str>) -> PreparedFactWrite {
        let message_id = new_id("msg");
        PreparedFactWrite {
            write: MemoryFactWrite {
                subject: "user".into(),
                predicate: "likes".into(),
                object: "tea".into(),
                confidence: 0.7,
                tags: vec!["preference".into()],
                source_ref: Some(FactSourceRef::from_message(
                    &message_id,
                    "extraction source snippet",
                )),
                durability: 0.6,
            },
            evidence: user_text.map(|user_text| FactEvidenceSource {
                message_id,
                user_text: user_text.into(),
                assistant_context: Some("Would you like tea or coffee?".into()),
            }),
        }
    }

    #[tokio::test]
    async fn exact_user_quote_supports_fact_and_raises_confidence_floor() {
        let inference = Arc::new(StubInference::new(
            r#"[{"candidate_index":0,"verdict":"supported","evidence_quote":"I like tea."}]"#,
        ));
        let verified = verify_fact_writes(
            inference.as_ref(),
            &Semaphore::new(1),
            vec![candidate(Some("I like tea."))],
        )
        .await;

        assert_eq!(verified[0].evidence_status, FactEvidenceStatus::Supported);
        assert_eq!(verified[0].evidence_quote.as_deref(), Some("I like tea."));
        assert_eq!(
            verified[0].write.source_ref.as_ref().unwrap().snippet,
            "I like tea."
        );
        assert!(verified[0].write.confidence >= VERIFIED_FACT_CONFIDENCE_FLOOR);
        assert_eq!(inference.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unsupported_quote_fails_closed_to_unverified() {
        let inference = Arc::new(StubInference::new(
            r#"[{"candidate_index":0,"verdict":"supported","evidence_quote":"I love coffee."}]"#,
        ));
        let verified = verify_fact_writes(
            inference.as_ref(),
            &Semaphore::new(1),
            vec![candidate(Some("I like tea."))],
        )
        .await;

        assert_eq!(verified[0].evidence_status, FactEvidenceStatus::Unverified);
        assert!(verified[0].evidence_quote.is_none());
        assert_eq!(verified[0].write.confidence, 0.7);
    }

    #[tokio::test]
    async fn absent_or_sensitive_evidence_never_reaches_verifier() {
        let inference = Arc::new(StubInference::new(
            r#"[{"candidate_index":0,"verdict":"supported","evidence_quote":"token=secret"}]"#,
        ));
        let verified = verify_fact_writes(
            inference.as_ref(),
            &Semaphore::new(1),
            vec![candidate(None), candidate(Some("api_key=secret"))],
        )
        .await;

        assert!(
            verified
                .iter()
                .all(|write| write.evidence_status == FactEvidenceStatus::Unverified)
        );
        assert_eq!(inference.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn uncertain_and_contradicted_verdicts_remain_distinct() {
        let inference = Arc::new(StubInference::new(
            r#"[{"candidate_index":0,"verdict":"uncertain"},{"candidate_index":1,"verdict":"contradicted"}]"#,
        ));
        let verified = verify_fact_writes(
            inference.as_ref(),
            &Semaphore::new(1),
            vec![
                candidate(Some("I like tea.")),
                candidate(Some("I like tea.")),
            ],
        )
        .await;

        assert_eq!(verified[0].evidence_status, FactEvidenceStatus::Uncertain);
        assert_eq!(
            verified[1].evidence_status,
            FactEvidenceStatus::Contradicted
        );
        assert!(verified.iter().all(|write| write.evidence_quote.is_none()));
    }

    #[tokio::test]
    async fn malformed_or_duplicate_verifier_results_fail_closed() {
        for response in [
            "not json",
            r#"[{"candidate_index":0,"verdict":"supported","evidence_quote":"I like tea."},{"candidate_index":0,"verdict":"supported","evidence_quote":"I like tea."}]"#,
        ] {
            let inference = Arc::new(StubInference::new(response));
            let verified = verify_fact_writes(
                inference.as_ref(),
                &Semaphore::new(1),
                vec![candidate(Some("I like tea."))],
            )
            .await;
            assert_eq!(verified[0].evidence_status, FactEvidenceStatus::Unverified);
        }
    }
}
