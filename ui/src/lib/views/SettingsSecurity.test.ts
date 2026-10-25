import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SettingsSecurity from './SettingsSecurity.svelte';
import type { SecuritySettingsDraft } from '$lib/contracts/settings.ts';
import type { SessionPermissionGrant } from '$lib/contracts/settings.ts';

function createSecurity(): SecuritySettingsDraft {
	return {
		execution_mode: 'collaborative',
		mxc_isolation_mode: 'strict',
		resource_policy: {
			files: {
				write_mode: 'workspace',
				write_roots: [],
				path_roots: [],
				read_only_roots: [],
				denied_paths: [],
			},
			network: { access: 'restricted', allowed_domains: [] },
		},
		execution_root: null,
		shell_runtime_read_roots: [],
		permissions: [{ key: 'files.delete', effect: 'deny' }],
	};
}

async function openResourceBoundaries() {}

describe('SettingsSecurity', () => {
	it('edits the new-session default directory independently from the resource policy', async () => {
		const security = createSecurity();
		const onPickExecutionRoot = vi.fn(async () => 'C:\\Projects\\selected');
		render(SettingsSecurity, { security, onPickExecutionRoot });
		await openResourceBoundaries();
		await fireEvent.input(screen.getByLabelText('只读保护路径'), {
			target: { value: 'C:\\Projects\\haven\\vendor' },
		});
		await fireEvent.input(screen.getByLabelText('拒绝访问路径'), {
			target: { value: 'C:\\Projects\\haven\\secrets' },
		});
		expect(security.resource_policy.files.read_only_roots).toEqual([
			'C:\\Projects\\haven\\vendor',
		]);
		expect(security.resource_policy.files.denied_paths).toEqual([
			'C:\\Projects\\haven\\secrets',
		]);
		await fireEvent.input(screen.getByLabelText('系统与工具链只读目录'), {
			target: { value: 'C:\\Python312\\Lib\nD:\\SDKs\\Windows Kits' },
		});
		expect(security.shell_runtime_read_roots).toEqual([
			'C:\\Python312\\Lib',
			'D:\\SDKs\\Windows Kits',
		]);

		await fireEvent.input(screen.getByLabelText('新会话默认目录'), {
			target: { value: 'C:\\Projects\\demo' },
		});

		expect(security.execution_root).toBe('C:\\Projects\\demo');
		expect(security.resource_policy?.files.write_roots).toEqual([]);
		expect(screen.getByText(/子进程是否隔离由各自的执行边界决定/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '选择目录' }));
		expect(onPickExecutionRoot).toHaveBeenCalledOnce();
		expect(security.execution_root).toBe('C:\\Projects\\selected');
	});

	it('shows configured write roots as part of the effective resource policy', async () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\workspace';
		security.resource_policy!.files.write_mode = 'roots';
		security.resource_policy!.files.write_roots = ['C:\\Projects\\write-a', 'D:\\Shared\\write-b'];
		render(SettingsSecurity, { security });
		await openResourceBoundaries();

		expect(screen.getByText('当前写入边界')).toBeTruthy();
		expect(screen.getByText('C:\\Projects\\write-a')).toBeTruthy();
		expect(screen.getByText('D:\\Shared\\write-b')).toBeTruthy();
		expect(screen.getByText(/工具级范围限制/)).toBeTruthy();
		expect(screen.queryByText('C:\\Projects\\workspace')).toBeNull();
	});

	it('shows the execution workspace as the write base when custom roots are empty', async () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\workspace';
		render(SettingsSecurity, { security });
		await openResourceBoundaries();

		expect(screen.getByText('C:\\Projects\\workspace')).toBeTruthy();
		expect(screen.getByText('当前写入边界')).toBeTruthy();
	});

	it('keeps the execution workspace draft when the native picker is cancelled', async () => {
		const security = createSecurity();
		security.execution_root = 'C:\\Projects\\existing';
		const onPickExecutionRoot = vi.fn(async () => null);
		render(SettingsSecurity, { security, onPickExecutionRoot });
		await openResourceBoundaries();

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
			resource_scope: [],
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
					resource_scope: [
						{
							kind: 'local_path',
							path: 'C:\\Projects\\demo',
							recursive: false,
							access: 'read',
						},
					],
				},
			],
			onResetSessionPermissions,
		});

		await fireEvent.click(screen.getByRole('button', { name: '清除 1 条会话授权' }));
		expect(screen.getByText(/清除 1 条会话允许与拒绝决定/)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '确认清除' }));
		expect(onResetSessionPermissions).toHaveBeenCalledOnce();
	});

	it('lets users switch the default execution preset', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('radio', { name: /无人值守 需要确认时停止/ }));
		expect(security.execution_mode).toBe('unattended');
	});

	it('lets users explicitly enable MXC compatibility isolation', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('radio', { name: /兼容隔离/ }));
		expect(security.mxc_isolation_mode).toBe('compatibility');
		expect(screen.getByText(/DACL 层会临时调整授权目录 ACL/)).toBeTruthy();
	});

	it('lets users choose the open network boundary from the resource policy', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('combobox', { name: '网络策略' }));
		await fireEvent.click(screen.getByRole('option', { name: '开放网络' }));
		expect(security.resource_policy?.network.access).toBe('open');
	});

	it.each([
		{ networkPolicy: 'deny', explanation: /这不是 Windows 防火墙规则/ },
		{
			networkPolicy: 'restricted',
			explanation: /无法按目的地检查的 MCP、技能和命令子进程会被拒绝/,
		},
		{
			networkPolicy: 'open',
			explanation: /Haven 不添加公网目标限制/,
		},
	] as const)(
		'explains $networkPolicy as a Haven policy rather than an OS firewall rule',
		async ({ networkPolicy, explanation }) => {
			const security = createSecurity();
			security.resource_policy!.network.access = networkPolicy;
			render(SettingsSecurity, { security });
			await openResourceBoundaries();

			expect(screen.getByText(explanation)).toBeTruthy();
		},
	);

	it('describes restricted destinations independently from approval behavior', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });
		await openResourceBoundaries();

		expect(security.resource_policy?.network.access).toBe('restricted');
		expect(screen.getByText(/是否询问由执行方式决定/)).toBeTruthy();
		expect(screen.getByText(/回环地址、私有网段和元数据地址会被拒绝/)).toBeTruthy();
		expect(screen.getByText('受限网络')).toBeTruthy();
	});

	it('explains that the domain list narrows destinations but does not enable networking', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });
		await openResourceBoundaries();

		expect(
			screen.getByText(/名单只增加目标限制，不会开启网络/),
		).toBeTruthy();
		expect(screen.getByText(/受限网络会阻止回环地址、私有网段和特殊用途地址/)).toBeTruthy();
	});

	it('explains the user-level file access and independent network policy', async () => {
		const security = createSecurity();
		security.resource_policy!.files.write_mode = 'unrestricted';
		render(SettingsSecurity, { security });
		await openResourceBoundaries();

		expect(
			screen.getByText(/移除 Haven 管理的路径范围限制/),
		).toBeTruthy();
		expect(screen.getByText(/仍受授权、风险规则和当前 Windows 用户权限约束/)).toBeTruthy();
	});

	it('describes the WorkspaceWrite MXC process boundary', async () => {
		render(SettingsSecurity, { security: createSecurity() });
		await openResourceBoundaries();

		expect(screen.getByText('文件写入范围')).toBeTruthy();
		expect(screen.getByText(/工具自己的资源限制只会进一步缩小范围/)).toBeTruthy();
		expect(screen.getByText(/工具自己的路径规则还会进一步收窄范围/)).toBeTruthy();
	});

	it.each([
		{
			writeMode: 'read_only',
			explanation: /拒绝本地文件写入/,
		},
		{
			writeMode: 'workspace',
			explanation: /写入限于当前会话工作目录/,
		},
	] as const)(
		'describes the $writeMode filesystem boundary',
		async ({ writeMode, explanation }) => {
			const security = createSecurity();
			security.resource_policy!.files.write_mode = writeMode;
			render(SettingsSecurity, { security });
			await openResourceBoundaries();

			expect(screen.getAllByText(explanation).length).toBeGreaterThan(0);
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
