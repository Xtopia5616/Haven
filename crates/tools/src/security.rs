mod contract;
mod engine;
mod execution;
mod filesystem;
mod prompt;
pub use contract::{
    AuthorizationCheck, AuthorizationDecision, AuthorizationExecutionProof, AuthorizationPort,
    AuthorizationReasonCode, AuthorizationRequest, ConfirmationReceipt,
};
pub use engine::AuthorizationEngine;
pub(crate) use execution::{
    ExecutionPolicyAuthorization, ExecutionPolicyPermit, execution_policy_denied,
};
pub use filesystem::is_safe_local_path;
pub(crate) use filesystem::{FileReadPolicy, FileWritePolicy};
#[cfg(test)]
pub(crate) use prompt::LOCAL_TOOL_SECURITY_MATRIX;
pub use prompt::permission_prompt_summary;
