const SENSITIVE_FIELD_KEY =
	/(^|[^a-z0-9])(?:api[_-]?key|access[_-]?key|auth(?:orization)?|bearer|client[_-]?secret|credential|cookie|password|passwd|private[_-]?key|refresh[_-]?token|secret|session[_-]?token|token)(?=$|[^a-z0-9])/i;

const INLINE_CREDENTIAL_PATTERNS = [
	/\b(?:api[_-]?key|access[_-]?token|client[_-]?secret|password|passwd|secret|token|authorization|cookie)\b\s*[:=]\s*["']?[^\s"'&,;]+/i,
	/--(?:api[_-]?key|access[_-]?token|client[_-]?secret|password|passwd|secret|token)(?:=|\s+)(?:"[^"]+"|'[^']+'|[^\s]+)/i,
	/\b[a-z][a-z0-9+.-]*:\/\/[^/\s:@]+:[^@\s/]+@/i,
	/\b(?:sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{12,}|github_pat_[A-Za-z0-9_]{12,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{20,})\b/,
	/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/,
	/-----BEGIN [A-Z ]*PRIVATE KEY-----/,
];

/** Best-effort detection for credential-like confirmation fields and values. */
export function isSensitivePermissionInputValue(key: string, value: unknown): boolean {
	if (SENSITIVE_FIELD_KEY.test(key)) return true;
	if (typeof value !== 'string') return false;
	return INLINE_CREDENTIAL_PATTERNS.some((pattern) => pattern.test(value));
}
