//! Cross-session messaging contracts and the shared JSONL transport.
//!
//! Agent supplies the in-process session mailbox/runtime adapter, while Tools
//! exposes the model-facing messaging tool over [`MessagingService`].

pub mod inbox;
pub mod messaging_service;

pub use messaging_service::{
    AgentControlOperation, AgentControlRequest, AgentControlResult, AgentSpawnRequest,
    AgentSpawnResult, MessageClaim, MessageTransport, MessagingRuntime, MessagingService,
    SentMessage, SessionMailbox, is_expired,
};
