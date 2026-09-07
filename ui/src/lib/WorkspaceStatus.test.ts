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

	it('shows readiness once backend bootstrap has completed', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
		});

		expect(screen.getByRole('button', { name: /就绪/ })).toBeTruthy();
	});
});
