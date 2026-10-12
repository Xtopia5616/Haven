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
import type {
	SessionPermissionGrant as GeneratedSessionPermissionGrant,
	TauriCommandRequest,
} from './contracts/generatedCommands.ts';
import { MEDIA_TIMEOUT_BOUNDS, type SessionPermissionGrant } from './contracts/settings.ts';
import { createSettingsFixture } from './contracts/settingsFixture.ts';

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
		const grants: GeneratedSessionPermissionGrant[] = [
			{
				session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
				session_title: 'Session',
				capability: 'files.read',
				target: 'operation',
				effect: 'allow',
			},
		];
		const rendererGrants: SessionPermissionGrant[] = [
			{
				sessionId: grants[0].session_id,
				session_title: grants[0].session_title,
				capability: grants[0].capability,
				target: grants[0].target,
				effect: grants[0].effect,
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
			.mockResolvedValueOnce('cred-0000008c3defb1edb984fe2ac71c71c7')
			.mockResolvedValueOnce('cred-000001187bdf63db7309fc558e38e38e')
			.mockResolvedValueOnce('cred-000001a4b9cf15c92c8efa8055555555')
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined)
			.mockResolvedValueOnce(undefined);

		await discardStagedCredentials();
		expect(await listSessionPermissions()).toEqual(rendererGrants);
		expect(await isAutostartEnabled()).toBe(true);
		expect(await runMemoryMaintenance()).toBe(4);
		await revokePermission('files.read');
		await revokeSessionPermission({
			sessionId: rendererGrants[0].sessionId,
			capability: rendererGrants[0].capability,
		});
		await resetPermissions();
		expect(await resetSessionPermissions()).toBe(2);
		await setHotkeyCaptureActive(true);
		expect(await stageProviderCredential({ providerName: 'provider', apiKey: 'secret' })).toBe(
			'cred-0000008c3defb1edb984fe2ac71c71c7',
		);
		expect(await stageOcrCredential({ apiSecret: false, value: 'key' })).toBe(
			'cred-000001187bdf63db7309fc558e38e38e',
		);
		expect(await stageOcrCredential({ apiSecret: true, value: 'secret' })).toBe(
			'cred-000001a4b9cf15c92c8efa8055555555',
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
					sessionId: rendererGrants[0].sessionId,
					capability: rendererGrants[0].capability,
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
			session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
		} as never);
		await expect(listSessionPermissions()).rejects.toThrow(
			'Invalid session permission grant identity',
		);
	});

	it.each([
		{ capability: 42 },
		{ session_title: 7 },
		{ target: 'future_target' },
		{ effect: 'future_effect' },
	])('rejects malformed session permission grant fields: %o', async (invalidFields) => {
		invokeMock.mockResolvedValueOnce([
			{
				session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
				session_title: 'Session',
				capability: 'files.read',
				target: 'operation',
				effect: 'allow',
				...invalidFields,
			},
		] as never);

		await expect(listSessionPermissions()).rejects.toThrow(
			'Invalid session permission grant at',
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

		for (const [kind, bounds] of Object.entries(MEDIA_TIMEOUT_BOUNDS)) {
			expect(() =>
				updateSettings({
					settings: { media: { [kind]: { timeout_secs: bounds.min - 1 } } },
				} as unknown as TauriCommandRequest<'update_settings'>),
			).toThrow(`Invalid Settings numeric value at settings.media.${kind}.timeout_secs`);
		}

		expect(invokeMock).not.toHaveBeenCalled();
	});
});
