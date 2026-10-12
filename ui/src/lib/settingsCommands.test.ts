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
import type { SessionPermissionGrant, TauriCommandRequest } from './contracts/generatedCommands.ts';
import { createSettingsFixture } from './contracts/settings-fixture.ts';

const invokeMock = vi.mocked(invoke);

describe('loadSettings', () => {
	beforeEach(() => invokeMock.mockReset());

	it('uses one command boundary, preserves future fields, and rejects unknown enum values', async () => {
		const payload = createSettingsFixture();
		payload.hotkey.key_binding = 'Ctrl+Alt+H';
		const responseWithFutureFields = {
			...payload,
			llm: { ...payload.llm, future_llm_field: { enabled: true } },
			future_settings_field: 'kept',
		};
		invokeMock.mockResolvedValue(responseWithFutureFields as never);

		await expect(loadSettings()).resolves.toBe(responseWithFutureFields);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_settings');

		const invalidPayload = { ...payload, hotkey: { ...payload.hotkey, mode: 'future_mode' } };
		invokeMock.mockResolvedValue(invalidPayload as never);
		await expect(loadSettings()).rejects.toThrow('Invalid get_settings response shape');
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
			.mockResolvedValueOnce('cred-11111111111111111111111111111111')
			.mockResolvedValueOnce('cred-22222222222222222222222222222222')
			.mockResolvedValueOnce('cred-33333333333333333333333333333333')
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
			'cred-11111111111111111111111111111111',
		);
		expect(await stageOcrCredential({ apiSecret: false, value: 'key' })).toBe(
			'cred-22222222222222222222222222222222',
		);
		expect(await stageOcrCredential({ apiSecret: true, value: 'secret' })).toBe(
			'cred-33333333333333333333333333333333',
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

	it('rejects a malformed autostart boolean response', async () => {
		invokeMock.mockResolvedValue('false' as never);

		await expect(isAutostartEnabled()).rejects.toThrow(
			'Invalid is_autostart_enabled boolean response',
		);
	});

	it('rejects session permission grants with non-canonical session identities', async () => {
		invokeMock.mockResolvedValueOnce([
			{ session_id: 'session-0123456789abcdef0123456789abcdef' },
		] as never);
		await expect(listSessionPermissions()).rejects.toThrow(
			'Invalid session permission grant identity',
		);

		invokeMock.mockResolvedValueOnce({
			session_id: 'ses-0123456789abcdef0123456789abcdef',
		} as never);
		await expect(listSessionPermissions()).rejects.toThrow(
			'Invalid session permission grant identity',
		);
	});

	it('rejects malformed opaque credential references from staging commands', async () => {
		invokeMock.mockResolvedValueOnce('cred-short' as never);
		await expect(
			stageProviderCredential({ providerName: 'provider', apiKey: 'secret' }),
		).rejects.toThrow('Invalid staged credential reference');

		invokeMock.mockResolvedValueOnce('plain-reference' as never);
		await expect(stageOcrCredential({ apiSecret: false, value: 'secret' })).rejects.toThrow(
			'Invalid staged credential reference',
		);
	});

	it('rejects unsafe Settings numbers before invoking the command', () => {
		expect(() =>
			updateSettings({
				settings: {
					context_limits: { max_upload_total_bytes: Number.MAX_SAFE_INTEGER + 1 },
				},
			} as unknown as TauriCommandRequest<'update_settings'>),
		).toThrow('Invalid Settings numeric value');
		expect(invokeMock).not.toHaveBeenCalled();
	});
});
