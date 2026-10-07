/** Format byte counts consistently across tool result cards. */
export function formatByteSize(value: unknown): string {
	const bytes = Number(value);
	if (!Number.isFinite(bytes) || bytes < 0) return '—';
	if (bytes < 1024) return `${bytes} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let size = bytes;
	let unitIndex = -1;
	while (size >= 1024 && unitIndex < units.length - 1) {
		size /= 1024;
		unitIndex++;
	}
	return `${size >= 100 ? size.toFixed(0) : size.toFixed(1)} ${units[unitIndex]}`;
}

/** Convert arbitrary numeric input to a CSS-safe percentage in [0, 100]. */
export function clampPercentage(value: unknown): number {
	const percentage = Number(value);
	if (!Number.isFinite(percentage)) return 0;
	return Math.max(0, Math.min(100, percentage));
}
