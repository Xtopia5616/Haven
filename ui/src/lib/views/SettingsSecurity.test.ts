import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SettingsSecurity from './SettingsSecurity.svelte';
import type { SecurityConfigInput } from '$lib/contracts/generatedCommands.ts';

function createSecurity(): SecurityConfigInput & {
	permissions: Array<{ key: string; effect: string }>;
} {
	return {
		permission_mode: 'default',
		sandbox_mode: 'workspace_write',
		network_policy: 'restricted',
		writable_roots: [],
		permissions: [{ key: 'files.delete', effect: 'deny' }],
	};
}

describe('SettingsSecurity', () => {
	it('uses a readable rule description and confirms clearing all rules in-app', async () => {
		const onResetPermissions = vi.fn(async () => true);
		render(SettingsSecurity, {
			security: createSecurity(),
			onResetPermissions,
		});

		expect(screen.getByText('文件操作 · 删除')).toBeTruthy();
		expect(screen.getByText('仅此操作')).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '清除 1 条永久规则' }));
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(screen.getByText(/这会清除 1 条/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '确认清除' }));
		expect(onResetPermissions).toHaveBeenCalledOnce();
	});

	it('shows and revokes session allow and deny grants by exact session', async () => {
		const grant = {
			session_id: 'ses-test',
			session_title: '测试对话',
			capability: 'files.write',
			target: 'operation',
			effect: 'deny' as const,
		};
		const onRevokeSessionPermission = vi.fn(async () => true);
		render(SettingsSecurity, {
			security: createSecurity(),
			sessionPermissions: [grant],
			onRevokeSessionPermission,
		});

		expect(screen.getByRole('list', { name: '已保存的会话授权' })).toBeTruthy();
		expect(screen.getByText('本会话拒绝')).toBeTruthy();
		expect(screen.getByText('测试对话')).toBeTruthy();
		await fireEvent.click(screen.getAllByRole('button', { name: '撤销' })[1]);
		expect(onRevokeSessionPermission).toHaveBeenCalledWith(grant);
	});

	it('states the exact impact when clearing session grants', async () => {
		const onResetSessionPermissions = vi.fn(async () => true);
		render(SettingsSecurity, {
			security: createSecurity(),
			sessionPermissions: [
				{
					session_id: 'ses-test',
					session_title: null,
					capability: 'files.write',
					target: 'operation',
					effect: 'allow',
				},
			],
			onResetSessionPermissions,
		});

		await fireEvent.click(screen.getByRole('button', { name: '清除 1 条会话授权' }));
		expect(screen.getByText(/清除 1 条会话允许与拒绝决定/)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '确认清除' }));
		expect(onResetSessionPermissions).toHaveBeenCalledOnce();
	});

	it('lets users switch the default policy from the visible choices', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('radio', { name: /自动 少打断/ }));
		expect(security.permission_mode).toBe('autonomous');
	});

	it('offers confirmation as the network default', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('button', { name: '网络策略' }));
		await fireEvent.click(screen.getByRole('option', { name: '请求确认' }));
		expect(security.network_policy).toBe('ask');
	});

	it('shows the disk and live security policy state after a partial apply failure', () => {
		render(SettingsSecurity, {
			security: createSecurity(),
			securityRuntimeStatus: 'unchanged',
			securityRuntimeNotice:
				'磁盘配置版本 4 已写入，但当前进程仍以最后完整应用的安全配置（版本 3）为基础；重启前实际采用该旧策略，之后单独确认的权限规则仍即时生效。',
		});

		expect(screen.getByText('仍按旧策略运行')).toBeTruthy();
		expect(screen.getByText(/重启前实际采用该旧策略/)).toBeTruthy();
	});
});
