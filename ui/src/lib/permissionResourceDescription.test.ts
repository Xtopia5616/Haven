import { describe, expect, it } from 'vitest';
import {
	declaredResourceDescription,
	permissionResourceScopeDescription,
} from './permissionResourceDescription.ts';

describe('permission resource descriptions', () => {
	it('distinguishes clipboard read and write resources', () => {
		expect(
			declaredResourceDescription({ kind: 'clipboard', access: 'read' }),
		).toBe('读取系统剪贴板');
		expect(
			permissionResourceScopeDescription({ kind: 'clipboard', access: 'write' }),
		).toBe('写入系统剪贴板');
	});

	it('shows the exact managed asset identity', () => {
		expect(
			declaredResourceDescription({
				kind: 'managed_asset',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
			}),
		).toBe('读取受管媒体资产 · asset-0123456789abcdef0123456789abcdef');
	});

	it('distinguishes desktop capture from concrete window control', () => {
		expect(
			declaredResourceDescription({ kind: 'desktop', access: 'capture' }),
		).toBe('截取桌面画面');
		expect(
			permissionResourceScopeDescription({
				kind: 'window',
				window_id: 'hwnd:2a',
				process_id: 42,
				access: 'interact',
				element_fingerprint: 'a'.repeat(64),
			}),
		).toBe('操作窗口 · hwnd:2a · PID 42 · 指定控件');
	});

	it('describes child process desktop handles separately from capture', () => {
		expect(declaredResourceDescription({ kind: 'desktop', access: 'process_handles' })).toBe(
			'子进程共享交互桌面，可访问窗口（剪贴板和输入注入仍受限制）',
		);
		expect(permissionResourceScopeDescription({ kind: 'desktop', access: 'process_handles' })).toBe(
			'子进程共享交互桌面，可访问窗口（剪贴板和输入注入仍受限制）',
		);
	});

	it('distinguishes Registry name enumeration from reading one value', () => {
		expect(
			declaredResourceDescription({
				kind: 'registry',
				hive: 'HKCU',
				path: 'Software\\Haven',
				access: 'enumerate_names',
				value_name: null,
			}),
		).toBe('列举注册表名称 · HKCU\\Software\\Haven');
		expect(
			permissionResourceScopeDescription({
				kind: 'registry',
				hive: 'HKLM',
				path: 'Software\\Haven',
				access: 'read_value',
				value_name: 'InstallPath',
			}),
		).toBe('读取注册表值 · HKLM\\Software\\Haven · InstallPath');
	});

	it('keeps MCP endpoint and argument fingerprints out of permission labels', () => {
		const resource = {
			kind: 'mcp_remote_invocation' as const,
			target_fingerprint: 'a'.repeat(64),
			request_fingerprint: 'b'.repeat(64),
		};
		expect(declaredResourceDescription(resource)).toBe(
			'MCP 远程调用 · 已绑定连接与精确参数',
		);
		expect(permissionResourceScopeDescription(resource)).toBe(
			'MCP 远程调用 · 已绑定连接与精确参数',
		);
	});
});
