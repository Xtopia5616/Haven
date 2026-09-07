//! Model-response policy for one ReAct turn.
//!
//! A provider stream produces a response, but that response is not yet a
//! turn result. Pi's loop treats empty/cut-off responses as a response-policy
//! concern, while Codex keeps the turn driver responsible for deciding when a
//! model attempt is complete. This module is that seam in Haven: it retries a
//! response without touching the durable transcript and returns one accepted
//! response to the turn coordinator.

use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::stream_step::StreamSession;
use super::{Action, ReActEngine, ReActState, RequestContext, StepCtx};
use haven_llm::LlmResponse;
use tokio_util::sync::CancellationToken;

/// The response that crossed the policy boundary and may now be projected or
/// dispatched to tools. Retries that remain empty never replace the previous
/// accepted candidate.
pub(super) struct AcceptedResponse {
    pub(super) response: LlmResponse,
    pub(super) thought: Option<String>,
    pub(super) actions: Vec<Action>,
    pub(super) empty_retries_remaining: u32,
}

pub(super) enum ResponseCycleOutcome {
    Accepted(Box<AcceptedResponse>),
    Cancelled,
    /// The provider could not complete a response-policy retry. The original
    /// cut-off text is not promoted to a final answer; the session remains
    /// continuable from its clean pre-response checkpoint.
    RetryableError(String),
}

fn response_candidate_has_payload(
    response: &LlmResponse,
    thought: &Option<String>,
    actions: &[Action],
) -> bool {
    thought.as_ref().is_some_and(|text| !text.trim().is_empty())
        || !actions.is_empty()
        || !response.web_search_calls.is_empty()
        || response
            .reasoning
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
        || !response.thinking_blocks.is_empty()
}

impl ReActEngine {
    /// Run the post-provider response policy.
    ///
    /// This method owns no durable state. `request_context` is an immutable
    /// provider view and `state` is read only here; the only mutable values are
    /// the in-process retry counters and the stream's output lifecycle. That
    /// makes it impossible for a failed/empty retry to leak a synthetic prompt
    /// into `events` or `canonical`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resolve_response_cycle(
        &self,
        ctx: &StepCtx,
        state: &ReActState,
        stream: &mut StreamSession<'_>,
        request_context: &RequestContext,
        mut response: LlmResponse,
        mut thought: Option<String>,
        mut actions: Vec<Action>,
        cancel: &CancellationToken,
        cut_off_retries: &mut u32,
        pending_ask: bool,
    ) -> ResponseCycleOutcome {
        let limits = self.limits();
        let mut empty_retries_remaining = limits.empty_response_max_retries;

        loop {
            let decision = self
                .hooks
                .after_llm(
                    self,
                    ctx,
                    super::hooks::AfterLlmInput {
                        thought: &thought,
                        actions: &actions,
                        response: &response,
                        canonical: &state.canonical,
                        state: ResponsePolicyState {
                            empty_retries_remaining,
                            empty_retry_delay_ms: limits.empty_response_retry_delay_ms,
                            cut_off_retries_used: *cut_off_retries,
                            cut_off_retries_max: limits.cut_off_retries,
                            pending_ask,
                        },
                    },
                )
                .await;

            let retry_context = match &decision {
                AfterLlmAction::RetryCutOff { nudge } => {
                    request_context.with_user_instruction((*nudge).to_owned())
                }
                AfterLlmAction::Accept | AfterLlmAction::RetryEmpty { .. } => {
                    request_context.clone()
                }
            };

            match decision {
                AfterLlmAction::Accept => {
                    return ResponseCycleOutcome::Accepted(Box::new(AcceptedResponse {
                        response,
                        thought,
                        actions,
                        empty_retries_remaining,
                    }));
                }
                AfterLlmAction::RetryEmpty { delay_ms } => {
                    empty_retries_remaining = empty_retries_remaining.saturating_sub(1);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return ResponseCycleOutcome::Cancelled,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }

                    match stream.retry(&retry_context).await {
                        Ok((retry_response, duration_ms)) => {
                            let (retry_thought, retry_actions) =
                                ReActEngine::parse_default_model_response(
                                    &retry_response,
                                    ctx.step_num,
                                );
                            self.record_step_usage(
                                ctx,
                                stream.role(),
                                &retry_response,
                                duration_ms,
                            )
                            .await;
                            if response_candidate_has_payload(
                                &retry_response,
                                &retry_thought,
                                &retry_actions,
                            ) {
                                thought = retry_thought;
                                actions = retry_actions;
                                response = retry_response;
                            }
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            return ResponseCycleOutcome::Cancelled;
                        }
                        Err(error) => {
                            if matches!(&error, haven_llm::LlmError::Cancelled) {
                                return ResponseCycleOutcome::Cancelled;
                            }
                            tracing::warn!(
                                session_id = %ctx.session_id,
                                step_number = ctx.step_num,
                                error = %error,
                                "empty-response retry failed"
                            );
                            return ResponseCycleOutcome::RetryableError(format!(
                                "empty-response retry failed: {error}"
                            ));
                        }
                    }
                }
                AfterLlmAction::RetryCutOff { .. } => {
                    *cut_off_retries += 1;
                    match stream.retry(&retry_context).await {
                        Ok((retry_response, duration_ms)) => {
                            let (retry_thought, retry_actions) =
                                ReActEngine::parse_default_model_response(
                                    &retry_response,
                                    ctx.step_num,
                                );
                            self.record_step_usage(
                                ctx,
                                stream.role(),
                                &retry_response,
                                duration_ms,
                            )
                            .await;
                            if response_candidate_has_payload(
                                &retry_response,
                                &retry_thought,
                                &retry_actions,
                            ) {
                                thought = retry_thought;
                                actions = retry_actions;
                                response = retry_response;
                            } else if *cut_off_retries >= limits.cut_off_retries {
                                return ResponseCycleOutcome::Accepted(Box::new(
                                    AcceptedResponse {
                                        response,
                                        thought,
                                        actions,
                                        empty_retries_remaining,
                                    },
                                ));
                            } else {
                                return ResponseCycleOutcome::RetryableError(
                                    "cut-off retry returned no usable response".into(),
                                );
                            }
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            return ResponseCycleOutcome::Cancelled;
                        }
                        Err(error) => {
                            if matches!(&error, haven_llm::LlmError::Cancelled) {
                                return ResponseCycleOutcome::Cancelled;
                            }
                            tracing::warn!(
                                session_id = %ctx.session_id,
                                step_number = ctx.step_num,
                                error = %error,
                                "cut-off retry failed"
                            );
                            return ResponseCycleOutcome::RetryableError(format!(
                                "cut-off retry failed: {error}"
                            ));
                        }
                    }
                }
            }
        }
    }
}
