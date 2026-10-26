//! Run boundary owned by the session supervisor.

use super::SessionRunHandler;
use haven_common::hooks::LifecycleHookRunSource;

/// Executes one complete ReAct run.  The supervisor owns admission and
/// lifecycle; this type owns only the run implementation boundary.
#[derive(Clone)]
pub(crate) struct SessionRunEngine {
    handler: SessionRunHandler,
}

impl SessionRunEngine {
    pub(crate) fn new(handler: SessionRunHandler) -> Self {
        Self { handler }
    }

    pub(crate) async fn run(
        self,
        session_id: String,
        run_source: LifecycleHookRunSource,
    ) -> anyhow::Result<()> {
        (self.handler)(session_id, run_source).await
    }
}
