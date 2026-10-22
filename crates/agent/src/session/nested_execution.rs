//! Durable checkpoints for child calls nested inside one session tool call.
//!
//! The append-only session event stream is authoritative. A started call with
//! no completed event is deliberately treated as unknown and is never replayed.

use haven_common::types::is_canonical_id;
use haven_memory::{SessionEvent, SessionStore};
use haven_tools::ToolResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

const NESTED_EXECUTION_EVENT_TYPE: &str = "nested_tool_call_checkpoint";
const MAX_NESTED_EXECUTION_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum NestedExecutionEvent {
    Started {
        parent_execution_id: String,
        call_index: u32,
        child_step_id: String,
        tool_name: String,
        input: Value,
    },
    Completed {
        parent_execution_id: String,
        call_index: u32,
        child_step_id: String,
        tool_name: String,
        input: Value,
        result: Box<ToolResult>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct CompletedNestedCall {
    pub child_step_id: String,
    pub result: ToolResult,
}

pub(super) struct NestedCallCompletion<'a> {
    pub(super) call_index: u32,
    pub(super) child_step_id: &'a str,
    pub(super) tool_name: &'a str,
    pub(super) input: &'a Value,
    pub(super) result: &'a ToolResult,
}

#[derive(Debug, Clone)]
struct NestedCallRecord {
    child_step_id: String,
    tool_name: String,
    input: Value,
    result: Option<ToolResult>,
}

pub(super) struct NestedExecutionJournal {
    store: SessionStore,
    session_id: String,
    parent_execution_id: String,
    records: Mutex<Option<Vec<NestedCallRecord>>>,
}

impl NestedExecutionJournal {
    pub(super) fn new(
        store: SessionStore,
        session_id: &str,
        parent_execution_id: &str,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "invalid nested session ID"
        );
        anyhow::ensure!(
            is_canonical_id(parent_execution_id, "step")
                || is_canonical_id(parent_execution_id, "toolrun"),
            "invalid nested parent execution ID"
        );
        Ok(Self {
            store,
            session_id: session_id.to_owned(),
            parent_execution_id: parent_execution_id.to_owned(),
            records: Mutex::new(None),
        })
    }

    /// Return an existing completed result, or the child identity to use for a
    /// new call. An in-flight checkpoint without completion is never replayed.
    pub(super) async fn prepare_call(
        &self,
        call_index: u32,
        proposed_step_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> Result<Result<String, CompletedNestedCall>, String> {
        self.load_records().await?;
        let records = self.records.lock().await;
        let records = records.as_ref().expect("journal was loaded");
        let index = call_index as usize;
        if index > records.len() {
            return Err("nested call replay skipped a durable checkpoint".into());
        }
        if let Some(record) = records.get(index) {
            if record.tool_name != tool_name || record.input != *input {
                return Err("nested call replay diverged from its durable checkpoint".into());
            }
            let Some(result) = record.result.clone() else {
                return Err(format!(
                    "nested operation '{}' has an unknown outcome and was not replayed",
                    record.tool_name
                ));
            };
            return Ok(Err(CompletedNestedCall {
                child_step_id: record.child_step_id.clone(),
                result,
            }));
        }
        if !is_canonical_id(proposed_step_id, "step") {
            return Err("nested child step identity is invalid".into());
        }
        Ok(Ok(proposed_step_id.to_owned()))
    }

    pub(super) async fn start_call(
        &self,
        call_index: u32,
        child_step_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> Result<(), String> {
        self.load_records().await?;
        let mut records = self.records.lock().await;
        let records = records.as_mut().expect("journal was loaded");
        if call_index as usize != records.len() {
            return Err("nested call start does not match the next durable checkpoint".into());
        }
        let event = NestedExecutionEvent::Started {
            parent_execution_id: self.parent_execution_id.clone(),
            call_index,
            child_step_id: child_step_id.to_owned(),
            tool_name: tool_name.to_owned(),
            input: input.clone(),
        };
        self.append_event(&event).await?;
        records.push(NestedCallRecord {
            child_step_id: child_step_id.to_owned(),
            tool_name: tool_name.to_owned(),
            input: input.clone(),
            result: None,
        });
        Ok(())
    }

    pub(super) async fn finish_call(
        &self,
        completion: NestedCallCompletion<'_>,
    ) -> Result<(), String> {
        self.load_records().await?;
        let mut records = self.records.lock().await;
        let records = records.as_mut().expect("journal was loaded");
        let Some(record) = records.get_mut(completion.call_index as usize) else {
            return Err("nested call completion has no durable start".into());
        };
        if record.result.is_some()
            || record.child_step_id != completion.child_step_id
            || record.tool_name != completion.tool_name
            || record.input != *completion.input
        {
            return Err("nested call completion does not match its durable start".into());
        }
        let event = NestedExecutionEvent::Completed {
            parent_execution_id: self.parent_execution_id.clone(),
            call_index: completion.call_index,
            child_step_id: completion.child_step_id.to_owned(),
            tool_name: completion.tool_name.to_owned(),
            input: completion.input.clone(),
            result: Box::new(completion.result.clone()),
        };
        self.append_event(&event).await?;
        record.result = Some(completion.result.clone());
        Ok(())
    }

    async fn append_event(&self, event: &NestedExecutionEvent) -> Result<(), String> {
        let payload = serde_json::to_vec(event)
            .map_err(|error| format!("nested call checkpoint could not be serialized: {error}"))?;
        if payload.len() > MAX_NESTED_EXECUTION_EVENT_BYTES {
            return Err("nested call checkpoint exceeds its persistence limit".into());
        }
        let payload = String::from_utf8(payload)
            .map_err(|error| format!("nested call checkpoint is invalid UTF-8: {error}"))?;
        self.store
            .append_domain_event(&self.session_id, NESTED_EXECUTION_EVENT_TYPE, &payload)
            .await
            .map_err(|error| format!("nested call checkpoint could not be persisted: {error}"))?;
        Ok(())
    }

    async fn load_records(&self) -> Result<(), String> {
        let mut records = self.records.lock().await;
        if records.is_some() {
            return Ok(());
        }
        let events = self
            .store
            .read_active_events_async(&self.session_id)
            .await
            .map_err(|error| format!("nested call checkpoints could not be read: {error}"))?;
        *records = Some(replay_records(&self.parent_execution_id, &events)?);
        Ok(())
    }
}

fn replay_records(
    parent_execution_id: &str,
    events: &[SessionEvent],
) -> Result<Vec<NestedCallRecord>, String> {
    let mut records: Vec<NestedCallRecord> = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.event_type == NESTED_EXECUTION_EVENT_TYPE)
    {
        let parsed: NestedExecutionEvent = serde_json::from_str(&event.payload)
            .map_err(|error| format!("nested call checkpoint is invalid: {error}"))?;
        match parsed {
            NestedExecutionEvent::Started {
                parent_execution_id: event_parent,
                call_index,
                child_step_id,
                tool_name,
                input,
            } if event_parent == parent_execution_id => {
                if !is_canonical_id(&child_step_id, "step") || call_index as usize != records.len()
                {
                    return Err("nested call checkpoint sequence is invalid".into());
                }
                records.push(NestedCallRecord {
                    child_step_id,
                    tool_name,
                    input,
                    result: None,
                });
            }
            NestedExecutionEvent::Completed {
                parent_execution_id: event_parent,
                call_index,
                child_step_id,
                tool_name,
                input,
                result,
            } if event_parent == parent_execution_id => {
                let Some(record) = records.get_mut(call_index as usize) else {
                    return Err("nested call completion has no durable start".into());
                };
                if !(record.result.is_none()
                    && record.child_step_id == child_step_id
                    && record.tool_name == tool_name
                    && record.input == input)
                {
                    return Err("nested call completion does not match its durable start".into());
                }
                record.result = Some(*result);
            }
            _ => {}
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_memory::{Database, SessionStore};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn journal_replays_completed_calls_and_refuses_unknown_started_calls() {
        let database = Arc::new(Database::open_in_memory().unwrap());
        let session = database.create_session("nested execution journal").unwrap();
        let store = SessionStore::new(database);
        let parent_id = "toolrun-1123456789abcdef0123456789abcdef";
        let child_step_id = "step-2123456789abcdef0123456789abcdef";
        let tool_name = "mcp__notes__read";
        let input = json!({"id": "note-1"});
        let journal =
            NestedExecutionJournal::new(store.clone(), &session.session_id, parent_id).unwrap();

        journal
            .start_call(0, child_step_id, tool_name, &input)
            .await
            .unwrap();
        let after_crash =
            NestedExecutionJournal::new(store.clone(), &session.session_id, parent_id).unwrap();
        let unknown = after_crash
            .prepare_call(0, child_step_id, tool_name, &input)
            .await
            .unwrap_err();
        assert!(unknown.contains("unknown outcome"));

        let result = ToolResult::ok(json!({"title": "Note"}));
        journal
            .finish_call(NestedCallCompletion {
                call_index: 0,
                child_step_id,
                tool_name,
                input: &input,
                result: &result,
            })
            .await
            .unwrap();
        let after_completion =
            NestedExecutionJournal::new(store, &session.session_id, parent_id).unwrap();
        let replay = after_completion
            .prepare_call(
                0,
                "step-3123456789abcdef0123456789abcdef",
                tool_name,
                &input,
            )
            .await
            .unwrap()
            .expect_err("completed child calls replay their durable result");
        assert_eq!(replay.child_step_id, child_step_id);
        assert_eq!(replay.result.output, json!({"title": "Note"}));
    }
}
