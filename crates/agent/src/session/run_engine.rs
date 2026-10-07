//! Run boundary owned by the session supervisor.

use super::SessionRunHandler;

/// Executes one complete ReAct run.  The supervisor owns admission and
/// lifecycle; this type owns only the run implementation boundary.
#[derive(Clone)]
pub struct SessionRunEngine {
    handler: SessionRunHandler,
}

impl SessionRunEngine {
    pub fn new(handler: SessionRunHandler) -> Self {
        Self { handler }
    }

    pub(crate) async fn run(self, session_id: String) -> anyhow::Result<()> {
        (self.handler)(session_id).await
    }
}
