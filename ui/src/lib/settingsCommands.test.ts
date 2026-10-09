import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	disableAutostart,
	discardStagedCredentials,
	enableAutostart,
	isAutostartEnabled,
	listSessionPermissions,
	loadSettings,
	resetPermissions,
	resetSessionPermissions,
	revokePermission,
	revokeSessionPermission,
	runMemoryMaintenance,
	setHotkeyCaptureActive,
	stageOcrCredential,
	stageProviderCredential,
	updateSettings,
} from './settingsCommands.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';
import type { SessionPermissionGrant } from './contracts/generatedCommands.ts';

const invokeMock = vi.mocked(invoke);

describe('loadSettings', () => {
	beforeEach(() => invokeMock.mockReset());

	it('uses one command boundary and preserves future fields and enum values', async () => {
		const payload = {
			hotkey: { key_binding: 'Ctrl+Alt+H', mode: 'future_mode' },
			llm: { models: [], future_llm_field: { enabled: true } },
			future_settings_field: 'kept',
		};
		invokeMock.mockResolvedValue(payload as never);

		await expect(loadSettings()).resolves.toBe(payload);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_settings');
	});

	it('preserves the existing no-op result for malformed root values', async () => {
		invokeMock.mockResolvedValue(null);
		await expect(loadSettings()).resolves.toBeNull();

		invokeMock.mockResolvedValue(['not', 'settings'] as never);
		await expect(loadSettings()).resolves.toBeNull();
	});
});

describe('Settings commands', () => {
	beforeEach(() => invokeMock.mockReset());

	it('forwards each Settings operation through its generated command owner', async () => {
		const grants: SessionPermissionGrant[] = [
			{
				session_id: 'ses-0123456789abcdef0123456789abcdef',
				session_title: 'Session',
				capability: 'files.read',
				target: 'operation',
				effect: 'allow',
			},
		];
		const settingsRequest = { settings: { default_shell: 'pwsh' as const } };
		invokeMock
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(grants)
			.mockResolvedValueOnce(true)
			.mockResolvedValueOnce(4)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(2)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce('cred-provider')
			.mockResolvedValueOnce('cred-ocr-key')
			.mockResolvedValueOnce('cred-ocr-secret')
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined);

		await discardStagedCredentials();
		expect(await listSessionPermissions()).toBe(grants);
		expect(await isAutostartEnabled()).toBe(true);
		expect(await runMemoryMaintenance()).toBe(4);
		await revokePermission('files.read');
		await revokeSessionPermission({
			sessionId: grants[0].session_id,
			capability: grants[0].capability,
		});
		await resetPermissions();
		expect(await resetSessionPermissions()).toBe(2);
		await setHotkeyCaptureActive(true);
		expect(await stageProviderCredential({ providerName: 'provider', apiKey: 'secret' })).toBe(
			'cred-provider',
		);
		expect(await stageOcrCredential({ apiSecret: false, value: 'key' })).toBe('cred-ocr-key');
		expect(await stageOcrCredential({ apiSecret: true, value: 'secret' })).toBe(
			'cred-ocr-secret',
		);
		await updateSettings(settingsRequest);
		await enableAutostart();
		await disableAutostart();

		expect(invokeMock.mock.calls).toEqual([
			['discard_staged_credentials'],
			['list_session_permissions'],
			['is_autostart_enabled'],
			['run_memory_maintenance'],
			['revoke_permission', { key: 'files.read' }],
			[
				'revoke_session_permission',
				{
					sessionId: grants[0].session_id,
					capability: grants[0].capability,
				},
			],
			['reset_permissions'],
			['reset_session_permissions'],
			['set_hotkey_capture_active', { active: true }],
			['stage_provider_credential', { providerName: 'provider', apiKey: 'secret' }],
			['stage_ocr_credential', { apiSecret: false, value: 'key' }],
			['stage_ocr_credential', { apiSecret: true, value: 'secret' }],
			['update_settings', settingsRequest],
			['enable_autostart'],
			['disable_autostart'],
		]);
	});

	it('propagates a rejected Settings operation unchanged', async () => {
		const failure = new Error('credential staging failed');
		invokeMock.mockRejectedValueOnce(failure);

		await expect(
			stageProviderCredential({ providerName: 'provider', apiKey: 'secret' }),
		).rejects.toBe(failure);
	});
});
