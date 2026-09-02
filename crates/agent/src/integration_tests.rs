use super::*;
use crate::types::{ConfirmPending, ConfirmPendingTool};
use async_trait::async_trait;
use futures_util::stream;
use haven_common::types::{
    CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart, InjectSource, RiskLevel,
};
use haven_llm::{
    FinishReason, LlmClient, LlmError, LlmResponse, StreamChunk, ToolDefinition, Usage,
};
use haven_tools::{Tool, ToolBox, ToolConcurrency, ToolResult, ToolsManager};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "integration_tests/canonical.rs"]
mod canonical;
#[path = "integration_tests/lifecycle.rs"]
mod lifecycle;
#[path = "integration_tests/react.rs"]
mod react;
#[path = "integration_tests/resume.rs"]
mod resume;
#[path = "integration_tests/rollback.rs"]
mod rollback;
#[path = "integration_tests/support.rs"]
mod support;
#[path = "integration_tests/tool_batch.rs"]
mod tool_batch;
#[path = "integration_tests/validation.rs"]
mod validation;
