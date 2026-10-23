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
});
