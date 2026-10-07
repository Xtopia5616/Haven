//! Model-response policy for one ReAct turn.
//!
//! A provider stream produces a response, but that response is not yet a
//! turn result. This module retries only structurally incomplete tool calls;
//! empty or abnormally terminated responses become recoverable session errors.

use super::retries::{AfterLlmAction, ResponsePolicyState};
use super::stream_step::StreamSession;
use super::{ReActEngine, ReActState, RequestContext, StepCtx, ToolCall};
use haven_llm::LlmResponse;
use tokio_util::sync::CancellationToken;

/// The response that crossed the policy boundary and may now be projected or
/// dispatched to tools.
pub(super) struct AcceptedResponse {
    pub(super) response: LlmResponse,
    pub(super) thought: Option<String>,
    pub(super) tool_calls: Vec<ToolCall>,
}

pub(super) enum ResponseCycleOutcome {
    Accepted(Box<AcceptedResponse>),
    Cancelled,
    /// The response is unsafe to accept or ended abnormally. The session
    /// remains continuable from its pre-response checkpoint, with any partial
    /// output preserved for recovery.
    RecoverableError(String),
}

impl ReActEngine {
    /// Run the post-provider response policy.
    ///
    /// This method owns no durable state. The only mutable values are the
    /// in-process structural retry counter and the stream's output lifecycle.
    /// Retry instructions stay in the provider request and never enter the
    /// durable transcript.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resolve_response_cycle(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        stream: &mut StreamSession<'_>,
        request_context: &RequestContext,
        mut response: LlmResponse,
        mut thought: Option<String>,
        mut tool_calls: Vec<ToolCall>,
        cancel: &CancellationToken,
        incomplete_tool_args_retries: &mut u32,
        pending_ask: bool,
    ) -> ResponseCycleOutcome {
        let limits = self.limits();

        loop {
            let decision = self
                .hooks
                .after_llm(
                    self,
                    ctx,
                    super::hooks::AfterLlmInput {
                        thought: &thought,
                        tool_calls: &tool_calls,
                        response: &response,
                        state: ResponsePolicyState {
                            incomplete_tool_args_retries_used: *incomplete_tool_args_retries,
                            incomplete_tool_args_retries_max: limits.incomplete_tool_args_retries,
                            pending_ask,
                        },
                    },
                )
                .await;

            match decision {
                AfterLlmAction::Accept => {
                    return ResponseCycleOutcome::Accepted(Box::new(AcceptedResponse {
                        response,
                        thought,
                        tool_calls,
                    }));
                }
                AfterLlmAction::Fail { reason } => {
                    self.persist_response_error(ctx, state, stream, &reason)
                        .await;
                    return ResponseCycleOutcome::RecoverableError(reason);
                }
                AfterLlmAction::RetryIncompleteToolArgs { nudge } => {
                    *incomplete_tool_args_retries += 1;
                    let retry_context = request_context.with_user_instruction(nudge);
                    match stream.retry(&retry_context).await {
                        Ok(retry_call) => {
                            let parsed_response = ReActEngine::parse_default_model_response(
                                &retry_call.response,
                                ctx.step_num,
                            );
                            self.record_step_usage(
                                ctx,
                                stream.request(),
                                &retry_call.response,
                                retry_call.duration_ms,
                                cancel.clone(),
                            )
                            .await;
                            response = retry_call.response;
                            thought = parsed_response.thought;
                            tool_calls = parsed_response.tool_calls;
                        }
                        Err(haven_llm::LlmError::Cancelled) => {
                            return ResponseCycleOutcome::Cancelled;
                        }
                        Err(error) => {
                            tracing::warn!(
                                session_id = %ctx.session_id,
                                step_number = ctx.step_num,
                                error = %error,
                                "incomplete-tool-arguments retry failed"
                            );
                            let reason = format!(
                                "工具参数结构重试失败：{error}；已保留当前输出，可点击“继续生成”重试。"
                            );
                            self.persist_response_error(ctx, state, stream, &reason)
                                .await;
                            return ResponseCycleOutcome::RecoverableError(reason);
                        }
                    }
                }
            }
        }
    }

    async fn persist_response_error(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        stream: &StreamSession<'_>,
        reason: &str,
    ) {
        let recovery = stream.persist_partial_on_error(state).await;
        if !recovery.should_discard() {
            tracing::error!(
                session_id = %ctx.session_id,
                step = ctx.step_num,
                reason,
                ?recovery,
                "response error also failed recovery persistence"
            );
        }
    }
}
