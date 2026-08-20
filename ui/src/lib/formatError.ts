/** Normalize unknown catch values into a short user-facing string. */
export function formatError(e: unknown): string {
	if (typeof e === 'string') return e;
	if (e instanceof Error && e.message) return e.message;
	if (e && typeof e === 'object' && 'message' in e) {
		const msg = (e as { message: unknown }).message;
		if (typeof msg === 'string' && msg) return msg;
	}
	try {
		return String(e);
	} catch {
		return '未知错误';
	}
}
