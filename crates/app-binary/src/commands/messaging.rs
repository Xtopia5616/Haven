//! Read-only cross-session messaging commands: agent registry + message
//! history for the "消息" tab. The bus itself lives in
//! `haven_tools::inbox`; these commands only read it, never write.

use crate::commands::log_err;
use haven_tools::inbox::{AgentInfo, Envelope, InboxBus};
use serde::Serialize;

/// Agent list with liveness, as seen by the messaging bus.
#[derive(Serialize)]
pub struct MessagingAgentsResponse {
    pub agents: Vec<AgentInfo>,
}

/// One agent's message history, newest first.
#[derive(Serialize)]
pub struct MessagingHistoryResponse {
    pub name: String,
    pub messages: Vec<Envelope>,
}

fn bus() -> InboxBus {
    InboxBus::default_root()
}

#[tauri::command]
pub async fn list_messaging_agents() -> Result<MessagingAgentsResponse, String> {
    let bus = bus();
    let agents = tokio::task::spawn_blocking(move || bus.list_agents())
        .await
        .map_err(|e| log_err("list_messaging_agents", e))?
        .map_err(|e| log_err("list_messaging_agents", e))?;
    Ok(MessagingAgentsResponse { agents })
}

#[tauri::command]
pub async fn get_messaging_history(
    name: String,
    limit: Option<usize>,
) -> Result<MessagingHistoryResponse, String> {
    let bus = bus();
    let name_for_bus = name.clone();
    let messages =
        tokio::task::spawn_blocking(move || bus.history(&name_for_bus, limit.unwrap_or(100)))
            .await
            .map_err(|e| log_err("get_messaging_history", e))?
            .map_err(|e| log_err("get_messaging_history", e))?;
    Ok(MessagingHistoryResponse { name, messages })
}
