import {
	parseSessionPermissionGrantResponses,
	parseSettingsPayload,
	parseStagedCredentialReference,
	validateSettingsNumbers,
	type SessionPermissionGrant,
	type SettingsPayload,
} from '$lib/contracts/settings.ts';
import { parseBooleanResponse } from '$lib/contracts/boolean.ts';
import { parseCountResponse } from '$lib/contracts/count.ts';
import type { TauriCommandRequest, TauriCommandResponse } from './contracts/generatedCommands.ts';
import { invoke } from '$lib/tauri.ts';

/** Load Settings through the single runtime boundary used by renderer callers. */
export function loadSettings(): Promise<SettingsPayload | null> {
	return invoke('get_settings').then(parseSettingsPayload);
}

/** Open the native folder picker for an execution workspace candidate. */
export function pickExecutionWorkspaceDirectory(): Promise<string | null> {
	return invoke('pick_execution_workspace_directory');
}

/** Drop staged credentials that were not included in a successful Settings save. */
export function discardStagedCredentials(): Promise<void> {
	return invoke('discard_staged_credentials');
}

/** Read the durable per-session permission grants shown by Settings. */
export function listSessionPermissions(): Promise<SessionPermissionGrant[]> {
	return invoke('list_session_permissions').then(parseSessionPermissionGrantResponses);
}

/** Read the current managed autostart state. */
export function isAutostartEnabled(): Promise<boolean> {
	return invoke('is_autostart_enabled').then((value: unknown) =>
		parseBooleanResponse(value, 'is_autostart_enabled'),
	);
}

/** Run the explicit Memory maintenance operation from Settings. */
export function runMemoryMaintenance(): Promise<number> {
	return invoke('run_memory_maintenance').then((value: unknown) =>
		parseCountResponse(value, 'run_memory_maintenance'),
	);
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
	return invoke('reset_session_permissions').then((value: unknown) =>
		parseCountResponse(value, 'reset_session_permissions'),
	);
}

/** Pause or resume the recording hotkey while Settings captures a key binding. */
export function setHotkeyCaptureActive(active: boolean): Promise<void> {
	return invoke('set_hotkey_capture_active', { active });
}

/** Stage a provider key in secure storage and return its opaque reference. */
export function stageProviderCredential(
	request: TauriCommandRequest<'stage_provider_credential'>,
): Promise<string> {
	return invoke('stage_provider_credential', request).then(parseStagedCredentialReference);
}

/** Begin Sign in with ChatGPT using Haven's local loopback callback. */
export function startChatGptPlanLogin(
	request: TauriCommandRequest<'start_chatgpt_plan_login'>,
): Promise<TauriCommandResponse<'start_chatgpt_plan_login'>> {
	return invoke('start_chatgpt_plan_login', request);
}

/** Validate the ChatGPT OAuth callback and stage its token record securely. */
export function completeChatGptPlanLogin(
	request: TauriCommandRequest<'complete_chatgpt_plan_login'>,
): Promise<TauriCommandResponse<'complete_chatgpt_plan_login'>> {
	return invoke('complete_chatgpt_plan_login', request);
}

/** Cancel one local ChatGPT login attempt and close its loopback listener. */
export function cancelChatGptPlanLogin(
	request: TauriCommandRequest<'cancel_chatgpt_plan_login'>,
): Promise<void> {
	return invoke('cancel_chatgpt_plan_login', request);
}

/** Delete one staged ChatGPT credential when its login result was abandoned. */
export function discardStagedChatGptPlanCredential(
	request: TauriCommandRequest<'discard_staged_chatgpt_plan_credential'>,
): Promise<void> {
	return invoke('discard_staged_chatgpt_plan_credential', request);
}

/** Move a staged ChatGPT credential when the Settings draft renames its provider. */
export function reassignStagedChatGptPlanCredential(
	request: TauriCommandRequest<'reassign_staged_chatgpt_plan_credential'>,
): Promise<void> {
	return invoke('reassign_staged_chatgpt_plan_credential', request);
}

/** Stage an Alibaba AI Search key in secure storage for one model config. */
export function stageIndependentSearchCredential(
	request: TauriCommandRequest<'stage_independent_search_credential'>,
): Promise<string> {
	return invoke('stage_independent_search_credential', request).then(parseStagedCredentialReference);
}

/** Stage an OCR key or secret in secure storage and return its opaque reference. */
export function stageOcrCredential(
	request: TauriCommandRequest<'stage_ocr_credential'>,
): Promise<string> {
	return invoke('stage_ocr_credential', request).then(parseStagedCredentialReference);
}

/** Persist a complete Settings update through the generated command contract. */
export function updateSettings(request: TauriCommandRequest<'update_settings'>): Promise<void> {
	validateSettingsNumbers(request.settings);
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
