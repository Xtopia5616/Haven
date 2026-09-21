import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import WorkspaceStatus from './WorkspaceStatus.svelte';

describe('WorkspaceStatus', () => {
	it('explains that browser preview has no Tauri backend', () => {
		render(WorkspaceStatus, { runtime: 'browser' });

		const status = screen.getByRole('status', { name: /浏览器预览/ });
		expect(status).toBeTruthy();
		expect(status.getAttribute('title')).toContain('Rust/Tauri 后端未启动');
	});

	it('shows readiness once backend bootstrap has completed', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
		});

		expect(screen.getByRole('status', { name: /就绪/ })).toBeTruthy();
	});

	it('shows the connection reason in the disconnected status title', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'disconnected',
			llmConnectionDetail: '网络请求失败，可能与网络、代理、DNS 或 TLS 有关',
		});

		const status = screen.getByRole('status', { name: /已断开/ });
		expect(status.getAttribute('title')).toContain('网络请求失败');
		expect(status.getAttribute('title')).toContain('检查 API 地址、API Key 和代理');
	});

	it('mirrors the active conversation and keeps the status dot moving', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			conversationStatus: '运行中',
			busySessions: new Set(['ses-1']),
		});

		expect(screen.getByRole('status', { name: /运行中/ })).toBeTruthy();
		expect(document.querySelector('.status-dot.animate')).toBeTruthy();
	});

	it('shows a paused conversation without animating it', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			conversationStatus: '已暂停',
		});

		expect(screen.getByRole('status', { name: /已暂停/ })).toBeTruthy();
		expect(document.querySelector('.status-dot.animate')).toBeNull();
	});

	it('keeps the task entry separate from the read-only status', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			runningActionCount: 2,
		});

		expect(screen.getByRole('status', { name: /后台任务/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: '打开任务' })).toBeTruthy();
		expect(document.querySelector('.status-badge')?.textContent).toBe('2');
	});
});
