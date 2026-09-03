/**
 * Shared semantic status palette for UI indicators and notifications.
 *
 * The values deliberately point at the existing M3 light/dark tokens so the
 * meaning stays stable while contrast adapts with the active theme.
 */
export type StatusTone = 'success' | 'warning' | 'error' | 'info' | 'tool' | 'neutral';

export type StatusColorTokens = {
	dot: string;
	background: string;
	foreground: string;
};

export const STATUS_COLORS: Record<StatusTone, StatusColorTokens> = {
	success: {
		dot: 'var(--md-sys-color-success)',
		background: 'var(--md-sys-color-success-container)',
		foreground: 'var(--md-sys-color-on-success-container)',
	},
	warning: {
		dot: 'var(--md-sys-color-warning)',
		background: 'var(--md-sys-color-warning-container)',
		foreground: 'var(--md-sys-color-on-warning-container)',
	},
	error: {
		dot: 'var(--md-sys-color-error)',
		background: 'var(--md-sys-color-error-container)',
		foreground: 'var(--md-sys-color-on-error-container)',
	},
	info: {
		dot: 'var(--md-sys-color-primary)',
		background:
			'color-mix(in srgb, var(--md-sys-color-primary) 8%, var(--md-sys-color-secondary-container))',
		foreground: 'var(--md-sys-color-on-secondary-container)',
	},
	tool: {
		dot: 'var(--md-sys-color-tertiary)',
		background: 'var(--md-sys-color-tertiary-container)',
		foreground: 'var(--md-sys-color-on-tertiary-container)',
	},
	neutral: {
		dot: 'var(--md-sys-color-outline)',
		background: 'var(--md-sys-color-surface-container-high)',
		foreground: 'var(--md-sys-color-on-surface-variant)',
	},
};

const STATUS_ALIASES: Record<string, StatusTone> = {
	primary: 'info',
	tertiary: 'tool',
	outline: 'neutral',
};

/** Resolve current component vocabulary to the shared semantic tone. */
export function resolveStatusTone(value: string | undefined | null): StatusTone {
	const normalized = value?.toLowerCase();
	if (normalized && normalized in STATUS_COLORS) {
		return normalized as StatusTone;
	}
	return (normalized && STATUS_ALIASES[normalized]) || 'neutral';
}

export function getStatusColorTokens(value: string | undefined | null): StatusColorTokens {
	return STATUS_COLORS[resolveStatusTone(value)];
}

export function getStatusDotColor(value: string | undefined | null): string {
	return getStatusColorTokens(value).dot;
}
