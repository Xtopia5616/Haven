import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import WorkspaceStatus from './WorkspaceStatus.svelte';

describe('WorkspaceStatus', () => {
	it('explains that browser preview has no Tauri backend', () => {
		render(WorkspaceStatus, { runtime: 'browser' });

		const status = screen.getByRole('status', { name: '状态：浏览器预览' });
		expect(status).toBeTruthy();
		expect(status.getAttribute('title')).toContain('Rust/Tauri 后端未启动');
		expect(screen.queryByRole('status', { name: /模型状态/ })).toBeNull();
	});

	it('shows an idle execution phase and hides a healthy model probe', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
		});

		expect(screen.getByRole('status', { name: '状态：空闲' })).toBeTruthy();
		expect(screen.queryByRole('status', { name: /模型状态/ })).toBeNull();
	});

	it('shows only the checking label before the first model probe completes', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: null,
		});

		expect(screen.getByRole('status', { name: '状态：检测中' })).toBeTruthy();
		expect(screen.queryByText('已配置')).toBeNull();
	});

	it('prioritizes an unconfigured model over idle', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'unconfigured',
		});

		expect(screen.getByRole('status', { name: '状态：模型未配置' })).toBeTruthy();
		expect(screen.queryAllByRole('status')).toHaveLength(1);
	});

	it('shows model unavailability and keeps the probe reason in its title', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'disconnected',
			llmConnectionDetail: '网络请求失败，可能与网络、代理、DNS 或 TLS 有关',
		});

		const status = screen.getByRole('status', { name: '状态：模型不可用' });
		expect(status.getAttribute('title')).toContain('网络请求失败');
		expect(status.getAttribute('title')).toContain('检查 API 地址、API Key 和代理');
	});

	it('lets model errors override routine ReAct progress and keeps it in the title', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'disconnected',
			executionPhase: 'generating',
			activeSessionStatusLabel: '运行中',
			busySessions: new Set(['ses-1']),
		});

		const status = screen.getByRole('status', { name: '状态：模型不可用' });
		expect(status.getAttribute('title')).toContain('执行状态：生成中');
		expect(document.querySelectorAll('[role="status"]')).toHaveLength(1);
		expect(document.querySelector('.status-dot.animate')).toBeNull();
	});

	it('uses the selected text for the request, response, and tool-result phases', () => {
		const labels = [
			['requesting', '请求中'],
			['waiting_response', '等待响应'],
			['waiting_result', '等待结果'],
		];
		for (const [executionPhase, label] of labels) {
			const { unmount } = render(WorkspaceStatus, {
				runtime: 'tauri',
				bootstrapReady: true,
				executionPhase,
				busySessions: new Set(['ses-1']),
			});
			expect(screen.getByRole('status', { name: `状态：${label}` })).toBeTruthy();
			unmount();
		}
	});

	it('uses unified waiting-operation and task labels from the active session', () => {
		for (const activeSessionStatusLabel of ['等待操作', '等待任务']) {
			const { unmount } = render(WorkspaceStatus, {
				runtime: 'tauri',
				bootstrapReady: true,
				activeSessionStatusLabel,
				busySessions: new Set(),
			});
			expect(
				screen.getByRole('status', { name: `状态：${activeSessionStatusLabel}` }),
			).toBeTruthy();
			unmount();
		}
	});

	it('shows a paused conversation without animating it', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			activeSessionStatusLabel: '已暂停',
		});

		expect(screen.getByRole('status', { name: '状态：已暂停' })).toBeTruthy();
		expect(document.querySelector('.status-dot.animate')).toBeNull();
	});

	it('keeps the task entry separate from the read-only status', () => {
		render(WorkspaceStatus, {
			runtime: 'tauri',
			bootstrapReady: true,
			llmConnected: 'ready',
			runningBackgroundToolRunCount: 2,
		});

		expect(screen.getByRole('status', { name: '状态：后台任务' })).toBeTruthy();
		expect(screen.getByRole('button', { name: '打开任务' })).toBeTruthy();
		expect(document.querySelector('.status-badge')?.textContent).toBe('2');
	});
});
