// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/agent/src/session.rs.
//
// statusColor() returns a hex color for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the memory/sessions page.
// InteractionRequest carries ask/confirm pause reasons; session status only
// exposes the generic paused state.
// isBusyStatus() covers dispatcher queue (pending) and claimed run (running).

/** Session statuses only. */
export const SESSION_STATUSES = [
	'pending',
	'running',
	'paused',
	'completed',
	'error',
] as const;

export type SessionStatus = (typeof SESSION_STATUSES)[number];

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
