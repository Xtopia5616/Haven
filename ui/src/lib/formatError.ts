const MAX_ERROR_MESSAGE_LENGTH = 240;

function redactSensitiveText(value: string): string {
	return value
		.replace(
			/((?:api[-_]?key|client[-_]?secret|access[-_]?token|authorization|password|secret|token|key)\s*[:=]\s*)[^\s,&;]+/gi,
			'$1[REDACTED]',
		)
		.replace(/\bBearer\s+[^\s,;]+/gi, 'Bearer [REDACTED]')
		.replace(/\b(?:sk-[A-Za-z0-9_-]{8,}|gsk_[A-Za-z0-9_-]{8,}|AIza[A-Za-z0-9_-]{20,}|xai-[A-Za-z0-9_-]{8,}|AKIA[A-Z0-9]{12,})\b/g, '[REDACTED]')
		.replace(/\b[A-Za-z]:[\\/][^\s,;"'()[\]{}]+/g, '[PATH]')
		.replace(/\\\\[^\s,;"'()[\]{}]+/g, '[PATH]');
}

function trimMessage(value: string): string {
	const normalized = redactSensitiveText(value)
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
		// Unknown objects are never serialized because their fields may contain
		// secrets, internal paths, or complete provider responses.
		return '未知错误';
	}
	return trimMessage(String(error));
}
