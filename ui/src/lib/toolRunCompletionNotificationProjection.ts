import type { AgentNotificationPayload } from './contracts/agent.ts';

export const MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS = 128;

export interface ToolRunCompletionNotificationGate {
	notify(payload: AgentNotificationPayload): void;
	settingsLoaded(): void;
}

/** Hold only ToolRun completion notifications until persisted settings are hydrated. */
export function createToolRunCompletionNotificationGate(
	deliver: (payload: AgentNotificationPayload) => void,
): ToolRunCompletionNotificationGate {
	let ready = false;
	const pending: AgentNotificationPayload[] = [];
	return {
		notify(payload) {
			if (!ready) {
				// Preserve the oldest events in order; while the queue is full, drop
				// new arrivals until persisted settings finish loading.
				if (pending.length >= MAX_PENDING_TOOL_RUN_COMPLETION_NOTIFICATIONS) return;
				pending.push(payload);
				return;
			}
			deliver(payload);
		},
		settingsLoaded() {
			if (ready) return;
			ready = true;
			for (const payload of pending.splice(0)) deliver(payload);
		},
	};
}

export interface ToolRunCompletionToast {
	message: string;
	type: 'info' | 'success' | 'error';
	durationMs: number;
}

/** Keep kind-specific completion presentation behind the shared notification owner. */
export function projectToolRunCompletionToast(
	payload: AgentNotificationPayload,
	activeSessionId: string | null,
): ToolRunCompletionToast | null {
	if (payload.notificationKind !== 'tool_run_completion') return null;
	if (payload.toolRunKind === 'background') {
		if (payload.toolRunStatus !== 'completed' && payload.toolRunStatus !== 'failed') return null;
		if (payload.sessionId && payload.sessionId === activeSessionId) return null;
		const completed = payload.toolRunStatus === 'completed';
		return {
			message: `后台任务${completed ? '完成' : '失败'}: ${payload.toolRunId}`,
			type: completed ? 'success' : 'error',
			durationMs: 4000,
		};
	}
	if (payload.toolRunKind === 'scheduled') {
		const title = payload.title || 'Haven';
		const body = payload.body || '新通知';
		return {
			message: title === 'Haven' ? body : `${title}: ${body}`,
			type: 'info',
			durationMs: 5000,
		};
	}
	return null;
}
