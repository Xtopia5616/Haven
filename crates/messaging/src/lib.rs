//! Cross-session messaging contracts and the shared JSONL transport.
//!
//! Agent supplies the in-process session mailbox/runtime adapter, while Tools
//! exposes the model-facing messaging tool over [`MessagingService`].

mod contract;
mod inbox;
mod messaging_service;

pub use contract::is_expired;
pub use inbox::{AgentInfo, AgentStatus, Envelope, MessageType, SendOutcome};

pub use messaging_service::{
    AgentControlOperation, AgentControlRequest, AgentControlResult, AgentSpawnRequest,
    AgentSpawnResult, MessageClaim, MessagingRuntime, MessagingService, SentMessage,
    SessionMailbox,
};

#[cfg(feature = "test-support")]
pub use messaging_service::{MessageTransport, file_transport_for_test};
