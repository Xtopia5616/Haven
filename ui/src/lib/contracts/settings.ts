/** Stable command responses used by the settings diagnostics UI. */

import type {
	ApiKeyStatus as GeneratedApiKeyStatus,
	LogInfo as GeneratedLogInfo,
	LogTail as GeneratedLogTail,
	ShellAvailability as GeneratedShellAvailability,
	Settings as GeneratedSettings,
	TauriCommandRequest,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isBoolean, isString } from './valueGuards.ts';

export type LogInfo = GeneratedLogInfo;
export type LogTail = GeneratedLogTail;
export type ShellAvailability = GeneratedShellAvailability;
export type ApiKeyStatus = GeneratedApiKeyStatus;

/** Exact Rust-owned config shape from the generated IPC contract. */
export type SettingsPayload = GeneratedSettings;

/** Rust-owned settings input shape, including Serde defaults for omitted fields. */
export type SettingsUpdatePayload = TauriCommandRequest<'update_settings'>['settings'];

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
		!isBoolean(value.enabled) ||
		!isString(value.level) ||
		(value.path !== null && !isString(value.path))
	) {
		throw new Error('invalid get_log_info response');
	}
	return { enabled: value.enabled, level: value.level, path: value.path };
}

export function parseLogTail(value: unknown): LogTail {
	if (!isRecord(value) || !isString(value.path) || !isString(value.content)) {
		throw new Error('invalid read_log_tail response');
	}
	return { path: value.path, content: value.content };
}

export function parseShellAvailability(value: unknown): ShellAvailability {
	if (!isRecord(value) || !isBoolean(value.available)) {
		throw new Error('invalid check_shell_available response');
	}
	return { available: value.available };
}

export function parseApiKeyStatus(value: unknown): ApiKeyStatus {
	const requiredFlags = ['stt', 'ocr', 'ocr_secret'] as const;
	if (
		!isRecord(value) ||
		!requiredFlags.every((key) => isBoolean(value[key])) ||
		!isRecord(value.models) ||
		!Object.values(value.models).every(isBoolean) ||
		!isRecord(value.providers) ||
		!Object.values(value.providers).every(isBoolean)
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
