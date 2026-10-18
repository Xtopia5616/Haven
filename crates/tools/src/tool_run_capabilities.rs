use crate::ToolRunCompletionReceiver;
use crate::{
    ToolRunAgentCapability, ToolRunCompletion, ToolRunCompletionCapability,
    ToolRunLifecycleEventSink, ToolRunListView, ToolRunManagementCapability, ToolRunRestoreSummary,
    ToolRunService, ToolRunView,
};
#[cfg(feature = "test-support")]
use crate::{ToolRunStatusView, ToolRunTestSupportPort};
use std::sync::Arc;

pub(crate) fn agent_port(service: Arc<ToolRunService>) -> Arc<dyn ToolRunAgentCapability> {
    Arc::new(SharedToolRunAgentCapability { service })
}

pub(crate) fn management_port(
    service: Arc<ToolRunService>,
) -> Arc<dyn ToolRunManagementCapability> {
    Arc::new(SharedToolRunManagementCapability { service })
}

#[cfg(feature = "test-support")]
pub(crate) fn test_support_port(service: Arc<ToolRunService>) -> Arc<dyn ToolRunTestSupportPort> {
    Arc::new(SharedToolRunTestSupportPort { service })
}
struct SharedToolRunAgentCapability {
    service: Arc<ToolRunService>,
}

#[async_trait::async_trait]
impl ToolRunAgentCapability for SharedToolRunAgentCapability {
    async fn start_background(
        &self,
        tool_name: String,
        source_step_id: Option<String>,
        admission_cancel: tokio_util::sync::CancellationToken,
        invocation: crate::ToolRunInvocation,
    ) -> anyhow::Result<String> {
        self.service
            .spawn_tool_invocation(tool_name, source_step_id, &admission_cancel, invocation)
            .await
    }

    async fn attach_session(&self, tool_run_id: &str, session_id: &str) {
        self.service.attach_session(tool_run_id, session_id).await;
    }

    async fn cancel_owned_background_by_session_checked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.service
            .cancel_owned_background_by_session_checked(session_id)
            .await
    }

    async fn cancel_owned_background_by_session(&self, session_id: &str) {
        self.service
            .cancel_owned_background_by_session(session_id)
            .await;
    }

    async fn list_for_session_views(&self, session_id: &str) -> Vec<ToolRunListView> {
        self.service.list_for_session_views(session_id).await
    }

    async fn restore(&self) -> ToolRunRestoreSummary {
        self.service.restore().await
    }

    async fn acknowledge_completion(&self, tool_run_result_id: &str) {
        self.service
            .acknowledge_tool_run_completion(tool_run_result_id)
            .await;
    }

    async fn acknowledge_unowned_completion(&self, tool_run_result_id: &str) {
        self.service
            .acknowledge_unowned_tool_run_completion(tool_run_result_id)
            .await;
    }

    fn take_completion_receiver(&self) -> Option<Box<dyn ToolRunCompletionCapability>> {
        Some(Box::new(SharedToolRunCompletionCapability {
            receiver: self.service.take_tool_run_receiver()?,
            service: Arc::clone(&self.service),
        }))
    }
}

struct SharedToolRunCompletionCapability {
    receiver: ToolRunCompletionReceiver,
    service: Arc<ToolRunService>,
}

#[async_trait::async_trait]
impl ToolRunCompletionCapability for SharedToolRunCompletionCapability {
    async fn recv_result_with_recovery(&mut self) -> Option<ToolRunCompletion> {
        self.receiver
            .recv_tool_run_result_with_recovery(&self.service)
            .await
    }
}

struct SharedToolRunManagementCapability {
    service: Arc<ToolRunService>,
}

#[async_trait::async_trait]
impl ToolRunManagementCapability for SharedToolRunManagementCapability {
    fn set_lifecycle_event_sink(&self, sink: ToolRunLifecycleEventSink) {
        self.service.set_lifecycle_event_sink(sink);
    }

    async fn shutdown(&self) {
        self.service.shutdown().await;
    }

    async fn board(&self) -> Vec<ToolRunView> {
        self.service.board().await
    }

    async fn list_board_history(&self) -> anyhow::Result<Vec<ToolRunView>> {
        self.service.list_board_history().await
    }

    async fn list_history(
        &self,
        query: crate::ToolRunHistoryQuery,
    ) -> anyhow::Result<Vec<ToolRunView>> {
        self.service.list_history(query).await
    }

    async fn cancel(&self, tool_run_id: &str) -> bool {
        self.service.cancel(tool_run_id).await
    }

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        self.service.delete_terminal(tool_run_id).await
    }

    async fn clear_terminal_history(&self) -> anyhow::Result<u64> {
        self.service.clear_terminal_history().await
    }
}

#[cfg(feature = "test-support")]
struct SharedToolRunTestSupportPort {
    service: Arc<ToolRunService>,
}

#[cfg(feature = "test-support")]
#[async_trait::async_trait]
impl ToolRunTestSupportPort for SharedToolRunTestSupportPort {
    async fn set_store(&self, store: Option<haven_memory::ToolRunStore>) {
        self.service.set_tool_run_store(store).await;
    }

    async fn status_view(&self, tool_run_id: &str) -> ToolRunStatusView {
        self.service.status_view(tool_run_id).await
    }

    async fn cancel(&self, tool_run_id: &str) -> bool {
        self.service.cancel(tool_run_id).await
    }

    async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        self.service.delete_terminal(tool_run_id).await
    }
}
