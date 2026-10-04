/** Stable command responses used by the settings diagnostics UI. */

import type {
  ApiKeyStatus as GeneratedApiKeyStatus,
  LogInfo as GeneratedLogInfo,
  LogTail as GeneratedLogTail,
  ShellAvailability as GeneratedShellAvailability,
  Settings as GeneratedSettings,
  TauriCommandRequest,
} from './generatedCommands.ts';

export type LogInfo = GeneratedLogInfo;
export type LogTail = GeneratedLogTail;
export type ShellAvailability = GeneratedShellAvailability;
export type ApiKeyStatus = GeneratedApiKeyStatus;

/** Open response shape for diagnostics so added metric fields remain available. */
export type MetricsSnapshot = Record<string, unknown>;

/** Exact Rust-owned config shape from the generated IPC contract. */
export type SettingsPayload = GeneratedSettings;

/** Rust-owned settings input shape, including Serde defaults for omitted fields. */
export type SettingsUpdatePayload = TauriCommandRequest<'update_settings'>['settings'];

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * The full Settings shape is owned by haven_common::config::Settings. Keep the
 * renderer shape tied to the generated Rust contract, while malformed root
 * values keep the existing no-op behavior.
 */
export function parseSettingsPayload(value: unknown): SettingsPayload | null {
	return isRecord(value) ? (value as unknown as SettingsPayload) : null;
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
