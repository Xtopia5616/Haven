// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/agent/src/session.rs.
//
// statusColor() returns a hex color for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the history page.
// isPausedStatus() covers both plain pause and ask-awaiting pause (F2).

export const ACTION_STATUSES = [
	'pending',
	'running',
	'paused',
	'paused_awaiting_answer',
	'completed',
	'failed',
	'error',
];

const COLOR_MAP: Record<string, string> = {
	pending: '#666',
	running: 'var(--md-sys-color-success)',
	paused: '#ccaa44',
	paused_awaiting_answer: '#ccaa44',
	completed: '#4488ff',
	failed: '#ff4444',
	error: '#ff4444',
};

const VARIANT_MAP: Record<string, string> = {
	pending: 'default',
	running: 'primary',
	paused: 'warning',
	paused_awaiting_answer: 'warning',
	completed: 'success',
	failed: 'error',
	error: 'error',
};

export function isPausedStatus(status: string | undefined | null): boolean {
	return status === 'paused' || status === 'paused_awaiting_answer';
}

export function statusColor(status: string) {
	return COLOR_MAP[status] || '#666';
}

export function statusVariant(status: string) {
	return VARIANT_MAP[status] || 'default';
}
