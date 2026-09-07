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

	it('mirrors the active conversation and keeps the status dot moving', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			conversationStatus: '运行中',
			busySessions: new Set(['ses-1']),
		});

		expect(screen.getByRole('button', { name: /运行中/ })).toBeTruthy();
		expect(document.querySelector('.status-dot.animate')).toBeTruthy();
	});

	it('shows a paused conversation without animating it', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			conversationStatus: '已暂停',
		});

		expect(screen.getByRole('button', { name: /已暂停/ })).toBeTruthy();
		expect(document.querySelector('.status-dot.animate')).toBeNull();
	});
});
