import { parseSettingsPayload, type SettingsPayload } from '$lib/contracts/settings.ts';
import type { SessionPermissionGrant, TauriCommandRequest } from './contracts/generatedCommands.ts';
import { invoke } from '$lib/tauri.ts';

/** Load Settings through the single runtime boundary used by renderer callers. */
export function loadSettings(): Promise<SettingsPayload | null> {
	return invoke('get_settings').then(parseSettingsPayload);
}

/** Drop staged credentials that were not included in a successful Settings save. */
export function discardStagedCredentials(): Promise<void> {
	return invoke('discard_staged_credentials');
}

/** Read the durable per-session permission grants shown by Settings. */
export function listSessionPermissions(): Promise<SessionPermissionGrant[]> {
	return invoke('list_session_permissions');
}

/** Read the current managed autostart state. */
export function isAutostartEnabled(): Promise<boolean> {
	return invoke('is_autostart_enabled');
}

/** Run the explicit Memory maintenance operation from Settings. */
export function runMemoryMaintenance(): Promise<number> {
	return invoke('run_memory_maintenance');
}

/** Revoke one permanent permission rule by its exact key. */
export function revokePermission(key: string): Promise<void> {
	return invoke('revoke_permission', { key });
}

/** Revoke one durable grant for the selected session and capability. */
export function revokeSessionPermission(
	request: TauriCommandRequest<'revoke_session_permission'>,
): Promise<void> {
	return invoke('revoke_session_permission', request);
}

/** Clear permanent permission rules while preserving session grants. */
export function resetPermissions(): Promise<void> {
	return invoke('reset_permissions');
}

/** Clear durable session grants and report the number removed. */
export function resetSessionPermissions(): Promise<number> {
	return invoke('reset_session_permissions');
}

/** Pause or resume the recording hotkey while Settings captures a key binding. */
export function setHotkeyCaptureActive(active: boolean): Promise<void> {
	return invoke('set_hotkey_capture_active', { active });
}

/** Stage a provider key in secure storage and return its opaque reference. */
export function stageProviderCredential(
	request: TauriCommandRequest<'stage_provider_credential'>,
): Promise<string> {
	return invoke('stage_provider_credential', request);
}

/** Stage an OCR key or secret in secure storage and return its opaque reference. */
export function stageOcrCredential(
	request: TauriCommandRequest<'stage_ocr_credential'>,
): Promise<string> {
	return invoke('stage_ocr_credential', request);
}

/** Persist a complete Settings update through the generated command contract. */
export function updateSettings(request: TauriCommandRequest<'update_settings'>): Promise<void> {
	return invoke('update_settings', request);
}

/** Enable the application-managed startup entry. */
export function enableAutostart(): Promise<void> {
	return invoke('enable_autostart');
}

/** Disable the application-managed startup entry. */
export function disableAutostart(): Promise<void> {
	return invoke('disable_autostart');
}
