/**
 * Stable command responses used by the settings diagnostics UI.
 *
 * These responses currently have no snake_case fields, but they still cross
 * the Tauri boundary and must be validated before the view consumes them.
 */

export interface LogInfo {
	enabled: boolean;
	level: string;
	path: string | null;
}

export interface LogTail {
	path: string;
	content: string;
}

export interface ShellAvailability {
	available: boolean;
}

export interface ApiKeyStatus {
	models: Record<string, boolean>;
	providers: Record<string, boolean>;
	stt: boolean;
	ocr: boolean;
	ocr_secret: boolean;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function parseLogInfo(value: unknown): LogInfo {
	if (
		!isRecord(value) ||
		typeof value.enabled !== 'boolean' ||
		typeof value.level !== 'string' ||
		(value.path !== null && typeof value.path !== 'string')
	) {
		throw new Error('invalid get_log_info response');
	}
	return { enabled: value.enabled, level: value.level, path: value.path };
}

export function parseLogTail(value: unknown): LogTail {
	if (!isRecord(value) || typeof value.path !== 'string' || typeof value.content !== 'string') {
		throw new Error('invalid read_log_tail response');
	}
	return { path: value.path, content: value.content };
}

export function parseShellAvailability(value: unknown): ShellAvailability {
	if (!isRecord(value) || typeof value.available !== 'boolean') {
		throw new Error('invalid check_shell_available response');
	}
	return { available: value.available };
}

export function parseApiKeyStatus(value: unknown): ApiKeyStatus {
	const requiredFlags = [
		'stt',
		'ocr',
		'ocr_secret',
	] as const;
	if (
		!isRecord(value) ||
		!requiredFlags.every((key) => typeof value[key] === 'boolean') ||
		!isRecord(value.models) ||
		!Object.values(value.models).every((entry) => typeof entry === 'boolean') ||
		!isRecord(value.providers) ||
		!Object.values(value.providers).every((entry) => typeof entry === 'boolean')
	) {
		throw new Error('invalid get_api_key_status response');
	}
	return {
		models: value.models as Record<string, boolean>,
		providers: value.providers as Record<string, boolean>,
		stt: value.stt as boolean,
		ocr: value.ocr as boolean,
		ocr_secret: value.ocr_secret as boolean,
	};
}
