import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import SettingsSecurity from './SettingsSecurity.svelte';

function createSecurity() {
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

		await fireEvent.click(screen.getByRole('button', { name: '清除所有规则' }));
		expect(screen.getByRole('dialog')).toBeTruthy();
		expect(screen.getByText(/这会清除所有/)).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '确认清除' }));
		expect(onResetPermissions).toHaveBeenCalledOnce();
	});

	it('lets users switch the default policy from the visible choices', async () => {
		const security = createSecurity();
		render(SettingsSecurity, { security });

		await fireEvent.click(screen.getByRole('radio', { name: /自动 少打断/ }));
		expect(security.permission_mode).toBe('autonomous');
	});
});
