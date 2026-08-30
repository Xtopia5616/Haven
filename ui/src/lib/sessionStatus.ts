// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/agent/src/session.rs.
//
// statusColor() returns a hex color for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the memory/sessions page.
// isPausedStatus() covers plain pause, ask-awaiting (F2), and confirm-awaiting (E3).
// isBusyStatus() covers dispatcher queue (pending) and claimed run (running).

/** Session statuses only (not background-action `failed`). */
export const SESSION_STATUSES = [
	'pending',
	'running',
	'paused',
	'paused_awaiting_answer',
	'paused_awaiting_confirm',
	'completed',
	'error',
];

/** @deprecated Use SESSION_STATUSES; kept for older imports. */
export const ACTION_STATUSES = SESSION_STATUSES;

const COLOR_MAP: Record<string, string> = {
	pending: '#666',
	running: 'var(--md-sys-color-success)',
	paused: '#ccaa44',
	paused_awaiting_answer: '#ccaa44',
	paused_awaiting_confirm: '#ccaa44',
	completed: '#4488ff',
	// Legacy / background-action status — sessions use `error`.
	failed: '#ff4444',
	error: '#ff4444',
};

const VARIANT_MAP: Record<string, string> = {
	pending: 'default',
	running: 'primary',
	paused: 'warning',
	paused_awaiting_answer: 'warning',
	paused_awaiting_confirm: 'warning',
	completed: 'success',
	failed: 'error',
	error: 'error',
};

export function isPausedStatus(status: string | undefined | null): boolean {
	return (
		status === 'paused' ||
		status === 'paused_awaiting_answer' ||
		status === 'paused_awaiting_confirm'
	);
}

/** Queued (`pending`) or claimed (`running`) — both block "idle" UI. */
export function isBusyStatus(status: string | undefined | null): boolean {
	return status === 'pending' || status === 'running';
}

export function statusColor(status: string) {
	return COLOR_MAP[status] || '#666';
}

export function statusVariant(status: string) {
	return VARIANT_MAP[status] || 'default';
}
