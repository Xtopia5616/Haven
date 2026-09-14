use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use super::platform;
use super::{WaitCondition, WindowParams, WindowTool};
use crate::ToolResult;

const DEFAULT_WAIT_SECS: u64 = 10;
const MAX_WAIT_SECS: u64 = 120;
const WAIT_POLL_MS: u64 = 200;
const WAIT_UI_POLL_MS: u64 = 500;

impl WindowTool {
    pub(super) async fn wait(
        &self,
        params: WindowParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let condition = params.condition.ok_or_else(|| {
            anyhow::anyhow!(
                "condition is required for wait (title_contains | foreground_contains | ui_text)"
            )
        })?;
        let text = params
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow::anyhow!("text is required for wait"))?
            .to_string();
        let timeout = params
            .timeout_secs
            .unwrap_or(DEFAULT_WAIT_SECS)
            .clamp(1, MAX_WAIT_SECS);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let title_filter = params.title.clone();
        let poll_ms = match condition {
            WaitCondition::UiText => WAIT_UI_POLL_MS,
            _ => WAIT_POLL_MS,
        };
        // One blocking wait session: reuse COM/UIA setup cost across polls.
        let cancel_flag = cancel.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                if cancel_flag.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let matched = match condition {
                    WaitCondition::TitleContains => platform::any_title_contains(&text)?,
                    WaitCondition::ForegroundContains => {
                        platform::foreground_title_contains(&text)?
                    }
                    WaitCondition::UiText => {
                        platform::any_ui_name_contains(title_filter.as_deref(), &text)?
                    }
                };
                if matched {
                    return Ok(ToolResult::ok(serde_json::json!({
                        "operation": "wait",
                        "waited": true,
                        "timed_out": false,
                        "matched": true,
                        "condition": condition,
                        "text": text,
                    })));
                }
                if Instant::now() >= deadline {
                    return Ok(ToolResult::ok(serde_json::json!({
                        "operation": "wait",
                        "waited": true,
                        "timed_out": true,
                        "matched": false,
                        "condition": condition,
                        "text": text,
                        "timeout_secs": timeout,
                    })));
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        })
        .await?
    }
}
