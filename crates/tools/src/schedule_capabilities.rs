use std::sync::Arc;

use crate::{
    ScheduleAgentCapability, ScheduleFiredReceiver, ScheduleManagementCapability, ScheduleService,
};

pub(crate) fn agent_port(service: Arc<ScheduleService>) -> Arc<dyn ScheduleAgentCapability> {
    Arc::new(SharedScheduleAgentCapability { service })
}

pub(crate) fn management_port(
    service: Arc<ScheduleService>,
) -> Arc<dyn ScheduleManagementCapability> {
    Arc::new(SharedScheduleManagementCapability { service })
}

struct SharedScheduleAgentCapability {
    service: Arc<ScheduleService>,
}

#[async_trait::async_trait]
impl ScheduleAgentCapability for SharedScheduleAgentCapability {
    async fn restore(&self) -> anyhow::Result<crate::ScheduleRestoreSummary> {
        self.service.restore().await
    }

    async fn cancel_owned_by_session_checked(&self, session_id: &str) -> anyhow::Result<()> {
        self.service
            .cancel_owned_by_session_checked(session_id)
            .await
    }

    async fn claim_execution(&self, schedule_run_id: &str, claim_id: &str) -> anyhow::Result<bool> {
        self.service
            .claim_execution(schedule_run_id, claim_id)
            .await
    }

    async fn release_execution(
        &self,
        schedule_run_id: &str,
        claim_id: &str,
    ) -> anyhow::Result<bool> {
        self.service
            .release_execution(schedule_run_id, claim_id)
            .await
    }

    async fn finish(
        &self,
        schedule_run_id: &str,
        status: haven_common::lifecycle::ScheduleRunStatus,
        result_summary: Option<&str>,
        error_reason: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.service
            .finish(schedule_run_id, status, result_summary, error_reason)
            .await
    }

    fn take_fired_receiver(&self) -> Option<Box<dyn ScheduleFiredReceiver>> {
        Some(Box::new(SharedScheduleFiredReceiver {
            receiver: self.service.take_fired_receiver()?,
        }))
    }

    async fn shutdown(&self) {
        self.service.shutdown().await;
    }
}

struct SharedScheduleFiredReceiver {
    receiver: tokio::sync::mpsc::Receiver<crate::ScheduleRunTriggered>,
}

#[async_trait::async_trait]
impl ScheduleFiredReceiver for SharedScheduleFiredReceiver {
    async fn recv(&mut self) -> Option<crate::ScheduleRunTriggered> {
        self.receiver.recv().await
    }
}

struct SharedScheduleManagementCapability {
    service: Arc<ScheduleService>,
}

#[async_trait::async_trait]
impl ScheduleManagementCapability for SharedScheduleManagementCapability {
    async fn create(
        &self,
        input: haven_memory::SchedulePlanInput,
    ) -> anyhow::Result<haven_memory::SchedulePlanRow> {
        self.service.create(input).await
    }

    async fn list_for_session(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<haven_memory::SchedulePlanRow>> {
        self.service.list_for_session(session_id).await
    }

    async fn list_enabled(&self) -> anyhow::Result<Vec<haven_memory::SchedulePlanRow>> {
        self.service.list_enabled().await
    }

    async fn cancel(&self, schedule_id: &str, session_id: &str) -> anyhow::Result<bool> {
        self.service.cancel(schedule_id, session_id).await
    }
}
