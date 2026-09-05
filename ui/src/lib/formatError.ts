const MAX_ERROR_MESSAGE_LENGTH = 240;

function trimMessage(value: string): string {
	const normalized = value
		.replace(/[\r\n\t]+/g, ' ')
		.replace(/\s{2,}/g, ' ')
		.trim();
	if (!normalized) return '未知错误';
	if (normalized.length <= MAX_ERROR_MESSAGE_LENGTH) return normalized;
	return `${normalized.slice(0, MAX_ERROR_MESSAGE_LENGTH - 1)}…`;
}

/** Normalize unknown catch values into a bounded, single-line UI string. */
export function formatError(error: unknown): string {
	if (error == null) return '未知错误';
	if (typeof error === 'string') return trimMessage(error);
	if (error instanceof Error && error.message) return trimMessage(error.message);
	if (typeof error === 'object') {
		const record = error as { message?: unknown; error?: unknown; reason?: unknown };
		for (const candidate of [record.message, record.error, record.reason]) {
			if (typeof candidate === 'string' && candidate.trim()) return trimMessage(candidate);
		}
		try {
			const serialized = JSON.stringify(error);
			if (serialized && serialized !== '{}') return trimMessage(serialized);
		} catch {
			// Fall through to the stable fallback below.
		}
		return '未知错误';
	}
	return trimMessage(String(error));
}
