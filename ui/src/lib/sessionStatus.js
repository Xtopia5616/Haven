// Canonical session status vocabulary + UI style mapping. The backend
// emits these strings via SessionStatus::as_str(); see crates/session/src/lib.rs.
//
// statusColor() returns a hex color for inline badges (SessionCard dot).
// statusVariant() returns a MaterialBadge variant for the history page.

export const ACTION_STATUSES = ['pending', 'running', 'paused', 'completed', 'failed', 'error'];

const COLOR_MAP = {
	pending: '#666',
	running: '#44cc44',
	paused: '#ccaa44',
	completed: '#4488ff',
	failed: '#ff4444',
	error: '#ff4444',
};

const VARIANT_MAP = {
	pending: 'default',
	running: 'primary',
	paused: 'warning',
	completed: 'success',
	failed: 'error',
	error: 'error',
};

export function statusColor(status) {
	return COLOR_MAP[status] || '#666';
}

export function statusVariant(status) {
	return VARIANT_MAP[status] || 'default';
}
