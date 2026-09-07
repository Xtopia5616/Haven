import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import WorkspaceStatus from './WorkspaceStatus.svelte';

describe('WorkspaceStatus', () => {
	it('explains that browser preview has no Tauri backend', () => {
		render(WorkspaceStatus, { runtime: 'browser' });

		const status = screen.getByRole('button', { name: /浏览器预览/ });
		expect(status).toBeTruthy();
		expect(status.getAttribute('title')).toContain('Rust/Tauri 后端未启动');
	});
});
