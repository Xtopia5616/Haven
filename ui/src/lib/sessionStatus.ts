// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/agent/src/session.rs.
//
// statusColor() returns a theme token for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the memory/sessions page.
// Paused sessions carry a derived waitingReason so views do not infer the
// cause by combining status, interactions, and ToolRun state.
// isBusyStatus() covers dispatcher queue (pending) and claimed run (running).

import {
	SESSION_WAITING_REASON_VALUES,
	type SessionWaitingReason,
} from './contracts/generatedCommands.ts';

const WAITING_REASON_LABELS: Record<SessionWaitingReason, string> = {
	user_input: '等待操作',
	user_interrupt: '已暂停',
	ask: '等待操作',
	confirmation: '等待操作',
	scheduled_confirmation: '等待操作',
	background_task: '等待任务',
	scheduled_task: '等待定时任务',
	step_budget: '等待操作',
	end_incomplete: '结束未完成，可重试',
};

const COLOR_MAP: Record<string, string> = {
	pending: 'var(--md-sys-color-outline)',
	running: 'var(--md-sys-color-primary)',
	paused: 'var(--md-sys-color-warning)',
	completed: 'var(--md-sys-color-success)',
	error: 'var(--md-sys-color-error)',
};

const VARIANT_MAP: Record<
	string,
	'default' | 'primary' | 'secondary' | 'success' | 'warning' | 'error'
> = {
	pending: 'default',
	running: 'primary',
	paused: 'warning',
	completed: 'success',
	error: 'error',
};

export function isPausedStatus(status: string | undefined | null): boolean {
	return status === 'paused';
}

function isSessionWaitingReason(value: unknown): value is SessionWaitingReason {
	return (SESSION_WAITING_REASON_VALUES as readonly unknown[]).includes(value);
}

export function waitingReasonLabel(reason: unknown): string | null {
	return isSessionWaitingReason(reason) ? WAITING_REASON_LABELS[reason] : null;
}

/** Read the normalized session field. IPC snake_case is mapped at the boundary. */
export function sessionWaitingReason(
	session:
		| {
				waitingReason?: unknown;
		  }
		| null
		| undefined,
): SessionWaitingReason | null {
	if (!session) return null;
	const value = session.waitingReason;
	return isSessionWaitingReason(value) ? value : null;
}

/** Queued (`pending`) or claimed (`running`) — both block "idle" UI. */
export function isBusyStatus(status: string | undefined | null): boolean {
	return status === 'pending' || status === 'running';
}

/** Terminal failure states that should remain read-only when history is opened. */
export function isErrorStatus(status: string | undefined | null): boolean {
	return status === 'error';
}

export function statusColor(status: string) {
	return COLOR_MAP[status] || 'var(--md-sys-color-outline)';
}

export function statusVariant(
	status: string,
): 'default' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' {
	return VARIANT_MAP[status] || 'default';
}
