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
	small_model: boolean;
	default_model: boolean;
	image_model: boolean;
	audio_model: boolean;
	embedding_model: boolean;
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
		'small_model',
		'default_model',
		'image_model',
		'audio_model',
		'embedding_model',
		'stt',
		'ocr',
		'ocr_secret',
	] as const;
	if (
		!isRecord(value) ||
		!requiredFlags.every((key) => typeof value[key] === 'boolean') ||
		!isRecord(value.providers) ||
		!Object.values(value.providers).every((entry) => typeof entry === 'boolean')
	) {
		throw new Error('invalid get_api_key_status response');
	}
	return {
		small_model: value.small_model as boolean,
		default_model: value.default_model as boolean,
		image_model: value.image_model as boolean,
		audio_model: value.audio_model as boolean,
		embedding_model: value.embedding_model as boolean,
		providers: value.providers as Record<string, boolean>,
		stt: value.stt as boolean,
		ocr: value.ocr as boolean,
		ocr_secret: value.ocr_secret as boolean,
	};
}
