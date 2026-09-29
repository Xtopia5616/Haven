// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/agent/src/session.rs.
//
// statusColor() returns a hex color for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the memory/sessions page.
// Paused sessions carry a derived waitingReason so views do not infer the
// cause by combining status, interactions, and action state.
// isBusyStatus() covers dispatcher queue (pending) and claimed run (running).

/** Session statuses only. */
export const SESSION_STATUSES = ['pending', 'running', 'paused', 'completed', 'error'] as const;

export type SessionStatus = (typeof SESSION_STATUSES)[number];

export const SESSION_WAITING_REASONS = [
	'user_input',
	'user_interrupt',
	'ask',
	'confirmation',
	'scheduled_confirmation',
	'background_task',
	'scheduled_task',
	'step_budget',
] as const;

export type SessionWaitingReason = (typeof SESSION_WAITING_REASONS)[number];

const WAITING_REASON_LABELS: Record<SessionWaitingReason, string> = {
	user_input: '等待操作',
	user_interrupt: '已暂停',
	ask: '等待操作',
	confirmation: '等待操作',
	scheduled_confirmation: '等待操作',
	background_task: '等待任务',
	scheduled_task: '等待定时任务',
	step_budget: '等待继续',
};

const COLOR_MAP: Record<string, string> = {
	pending: '#666',
	running: 'var(--md-sys-color-success)',
	paused: '#ccaa44',
	completed: '#4488ff',
	error: '#ff4444',
};

const VARIANT_MAP: Record<string, string> = {
	pending: 'default',
	running: 'primary',
	paused: 'warning',
	completed: 'success',
	error: 'error',
};

export function isPausedStatus(status: string | undefined | null): boolean {
	return status === 'paused';
}

export function isSessionWaitingReason(value: unknown): value is SessionWaitingReason {
	return (SESSION_WAITING_REASONS as readonly unknown[]).includes(value);
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
	return COLOR_MAP[status] || '#666';
}

export function statusVariant(status: string) {
	return VARIANT_MAP[status] || 'default';
}
