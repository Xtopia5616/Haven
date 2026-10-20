import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SettingsSecurity from './SettingsSecurity.svelte';
import type { SecurityConfigInput } from '$lib/contracts/generatedCommands.ts';
import type { SessionPermissionGrant } from '$lib/contracts/settings.ts';

function createSecurity(): SecurityConfigInput & {
	permissions: Array<{ key: string; effect: string }>;
} {
	return {
		permission_mode: 'default',
		sandbox_mode: 'workspace_write',
		network_policy: 'restricted',
		execution_root: null,
		writable_roots: [],
		permissions: [{ key: 'files.delete', effect: 'deny' }],
	};
}

describe('SettingsSecurity', () => {
	it('edits the execution workspace independently from writable roots', async () => {
		const security = createSecurity();
		const onPickExecutionRoot = vi.fn(async () => 'C:\\Projects\\selected');
		render(SettingsSecurity, { security, onPickExecutionRoot });

		await fireEvent.input(screen.getByLabelText('执行工作区目录'), {
			target: { value: 'C:\\Projects\\demo' },
		});

		expect(security.execution_root).toBe('C:\\Projects\\demo');
		expect(security.writable_roots).toEqual([]);
		expect(screen.getByText(/子进程是否隔离由各自的执行边界决定/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '选择目录' }));
		expect(onPickExecutionRoot).toHaveBeenCalledOnce();
		expect(security.execution_root).toBe('C:\\Projects\\selected');
	});

	it('shows custom writable roots as the effective workspace-write base', () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\workspace';
		security.writable_roots = ['C:\\Projects\\write-a', 'D:\\Shared\\write-b'];
		render(SettingsSecurity, { security });

		expect(screen.getByText('工作区可写模式的写入基准')).toBeTruthy();
		expect(screen.getByText('C:\\Projects\\write-a')).toBeTruthy();
		expect(screen.getByText('D:\\Shared\\write-b')).toBeTruthy();
		expect(screen.getByText(/allowed_paths 取交集/)).toBeTruthy();
		expect(screen.queryByText('C:\\Projects\\workspace')).toBeNull();
	});

	it('shows the execution workspace as the write base when custom roots are empty', () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\workspace';
		render(SettingsSecurity, { security });

		expect(screen.getByText('C:\\Projects\\workspace')).toBeTruthy();
		expect(screen.getByText('工作区可写模式的写入基准')).toBeTruthy();
	});

	it('keeps the execution workspace draft when the native picker is cancelled', async () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\existing';
		const onPickExecutionRoot = vi.fn(async () => null);
		render(SettingsSecurity, { security, onPickExecutionRoot });

		await fireEvent.click(screen.getByRole('button', { name: '选择目录' }));

		expect(onPickExecutionRoot).toHaveBeenCalledOnce();
		expect(security.execution_root).toBe('C:\\Projects\\existing');
	});

	it('uses a readable rule description and confirms clearing all rules in-app', async () => {
		const onResetPermissions = vi.fn(async () => true);
		render(SettingsSecurity, {
			security: createSecurity(),
			onResetPermissions,
		});

		expect(screen.getByText('文件操作 · 删除')).toBeTruthy();
		expect(
			screen.getByText(
				/匹配“files.delete”本身及所有以“files.delete.”开头的下级操作或工具能力/,
			),
		).toBeTruthy();
		expect(screen.getByText(/拒绝优先于任何匹配的允许规则/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '清除 1 条永久规则' }));
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(screen.getByText(/这会清除 1 条/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '确认清除' }));
		expect(onResetPermissions).toHaveBeenCalledOnce();
	});

	it('shows and revokes session allow and deny grants by exact session', async () => {
		const grant: SessionPermissionGrant = {
			sessionId: 'ses-f598facda111677e0871bdc09d391332',
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
		expect(
			screen.getByText(
				'只对列出的会话生效。允许和拒绝决定都保存在本机，不会按时间自动到期；会话重新打开时仍会恢复。逐条撤销、整组清除或删除会话（含保留期清理）后移除。',
			),
		).toBeTruthy();
		expect(screen.getByText('本会话拒绝')).toBeTruthy();
		expect(screen.getByText('目标层级：单项操作')).toBeTruthy();
		expect(screen.getAllByText(/拒绝优先于任何匹配的允许规则/)).toHaveLength(2);
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
					sessionId: 'ses-f598facda111677e0871bdc09d391332',
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

	it('lets users choose Ask from the network policy menu', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('button', { name: '网络策略' }));
		await fireEvent.click(screen.getByRole('option', { name: '请求确认' }));
		expect(security.network_policy).toBe('ask');
	});

	it.each([
		{ networkPolicy: 'deny', explanation: /这不是 Windows 防火墙规则/ },
		{ networkPolicy: 'ask', explanation: /子进程仍按当前 Windows 用户权限联网/ },
		{
			networkPolicy: 'restricted',
			explanation: /无法按目的地检查的 MCP、技能和命令子进程会被拒绝/,
		},
		{
			networkPolicy: 'open',
			explanation: /获准启动的子进程按当前 Windows 用户权限联网/,
		},
	] as const)(
		'explains $networkPolicy as a Haven policy rather than an OS firewall rule',
		({ networkPolicy, explanation }) => {
			const security = createSecurity();
			security.network_policy = networkPolicy;
			render(SettingsSecurity, { security });

			expect(screen.getByText(explanation)).toBeTruthy();
		},
	);

	it('describes Ask behavior across workspace and full-access modes', () => {
		const security = createSecurity();
		security.network_policy = 'ask';
		render(SettingsSecurity, { security });

		expect(security.network_policy).toBe('ask');
		expect(
			screen.getByText(/工作区可写模式会拦截无法验证目的地的 MCP、技能和命令子进程/),
		).toBeTruthy();
	});

	it('explains the user-level file access and independent network policy', () => {
		const security = createSecurity();
		security.sandbox_mode = 'full_access';
		render(SettingsSecurity, { security });

		expect(
			screen.getByText(/子进程以当前 Windows 用户权限访问该用户可访问的文件/),
		).toBeTruthy();
		expect(screen.getByText(/每棵受管进程树限制为最多 256 个活动进程/)).toBeTruthy();
	});

	it('describes file access modes as Haven authorization rather than an OS sandbox', () => {
		render(SettingsSecurity, { security: createSecurity() });

		expect(screen.getByText('文件访问模式')).toBeTruthy();
		expect(screen.getByText(/不会替其它进程设置 Windows 文件或网络限制/)).toBeTruthy();
		expect(screen.getByText(/无法由执行边界约束文件访问的子进程仍会被拒绝/)).toBeTruthy();
	});

	it.each([
		{
			sandboxMode: 'read_only',
			explanation: /不会仅因只读模式而一概拒绝网络请求、打开外链等外部效果/,
		},
		{
			sandboxMode: 'workspace_write',
			explanation: /不会仅因目标不在可写根内而拒绝其它外部效果/,
		},
	] as const)(
		'$sandboxMode keeps external effects on their own authorization path',
		({ sandboxMode, explanation }) => {
			const security = createSecurity();
			security.sandbox_mode = sandboxMode;
			render(SettingsSecurity, { security });

			expect(screen.getByText(explanation)).toBeTruthy();
			expect(screen.getByText(/网络策略、操作风险与确认规则处理/)).toBeTruthy();
		},
	);

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
