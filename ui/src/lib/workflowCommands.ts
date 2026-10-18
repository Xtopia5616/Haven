import { invoke } from '$lib/tauri.ts';
import type { TauriCommandRequest, TauriCommandResponse } from './contracts/generatedCommands.ts';

export type WorkflowDefinitionDto = TauriCommandResponse<'list_workflow_definitions'>[number];
export type WorkflowScheduleDto = TauriCommandResponse<'list_workflow_schedules'>[number];

export function listWorkflowDefinitions(): Promise<WorkflowDefinitionDto[]> {
	return invoke('list_workflow_definitions');
}

export function saveWorkflowDefinition(
	request: TauriCommandRequest<'save_workflow_definition'>,
): Promise<WorkflowDefinitionDto> {
	return invoke('save_workflow_definition', request);
}

export function deleteWorkflowDefinition(workflowId: string): Promise<boolean> {
	return invoke('delete_workflow_definition', { workflowId });
}

export function runWorkflowDefinition(workflowId: string, sessionId: string): Promise<string> {
	return invoke('run_workflow_definition', { workflowId, sessionId });
}

export function retryWorkflowRun(toolRunId: string): Promise<string | null> {
	return invoke('retry_workflow_run', { toolRunId });
}

export function listWorkflowSchedules(): Promise<WorkflowScheduleDto[]> {
	return invoke('list_workflow_schedules');
}

export function createWorkflowSchedule(
	request: TauriCommandRequest<'create_workflow_schedule'>,
): Promise<WorkflowScheduleDto> {
	return invoke('create_workflow_schedule', request);
}

export function setWorkflowScheduleEnabled(scheduleId: string, enabled: boolean): Promise<boolean> {
	return invoke('set_workflow_schedule_enabled', { scheduleId, enabled });
}

export function deleteWorkflowSchedule(scheduleId: string): Promise<boolean> {
	return invoke('delete_workflow_schedule', { scheduleId });
}
