import { fireEvent, render, screen } from '@testing-library/svelte';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import SessionTimeline from './SessionTimeline.svelte';
import welcomeSource from './SessionWelcome.svelte?raw';

const welcomeStyles = welcomeSource.match(/<style>([\s\S]*?)<\/style>/)?.[1];
const testStyleElement = document.createElement('style');

beforeAll(() => {
	if (!welcomeStyles) throw new Error('SessionWelcome component styles are missing');
	testStyleElement.textContent = welcomeStyles;
	document.head.append(testStyleElement);
});

afterAll(() => testStyleElement.remove());

describe('SessionTimeline', () => {
	it('keeps the empty-state welcome content centered in the full message viewport', () => {
		render(SessionTimeline, { messages: [] });

		const welcome = document.querySelector('.welcome');
		expect(welcome).toBeTruthy();
		expect(getComputedStyle(welcome!).minHeight).toBe('100%');
	});

	it('fills an example prompt without sending and offers model setup only when needed', async () => {
		const onWelcomePrompt = vi.fn();
		const onConfigureModel = vi.fn();
		render(SessionTimeline, {
			messages: [],
			showModelSetup: true,
			onWelcomePrompt,
			onConfigureModel,
		});

		const prompt = '请把这段会议记录整理成摘要和待办清单。';
		await fireEvent.click(screen.getByRole('button', { name: prompt }));
		expect(onWelcomePrompt).toHaveBeenCalledTimes(1);
		expect(onWelcomePrompt).toHaveBeenCalledWith(prompt);
		expect(onConfigureModel).not.toHaveBeenCalled();
		expect(screen.getByText('选择示例只会填入输入框，确认后再发送。')).toBeTruthy();

		await fireEvent.click(screen.getByRole('button', { name: '配置模型' }));
		expect(onConfigureModel).toHaveBeenCalledTimes(1);
	});

	it('renders the message timeline immediately when the first message arrives', () => {
		render(SessionTimeline, {
			messages: [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: '你好' }],
		});

		expect(document.querySelector('.message-list')).toBeTruthy();
		expect(document.querySelector('.bubble.user')?.textContent).toContain('你好');
	});

	it('keeps a pending permission decision visible beside the active transcript', () => {
		render(SessionTimeline, {
			messages: [
				{
					id: 'step-f5ba153deb417361f0e17158c2c3924a',
					role: 'assistant' as const,
					content: '正在申请权限',
					type: 'thought' as const,
					stepNumber: 1,
					streaming: false,
				},
			],
			confirmationAwaiting: true,
		});

		expect(screen.getByRole('status').textContent).toContain('等待权限确认');
		expect(screen.getByRole('status').textContent).toContain('选择允许或拒绝');
	});

	it('renders waiting background progress inside the toolrun.start card', () => {
		const { container } = render(SessionTimeline, {
			messages: [
				{
					id: 'step-f5ba153deb417361f0e17158c2c3924a',
					role: 'assistant' as const,
					content:
						'{"tool_run_id":"toolrun-00000000000000000000000000000001","tool_name":"files.write"}',
					type: 'tool' as const,
					toolName: 'toolrun.start',
					sourceToolRunId: 'toolrun-00000000000000000000000000000001',
					stepNumber: 2,
					streaming: false,
				},
			],
			sessionToolRuns: [
				{
					toolRunId: 'toolrun-00000000000000000000000000000001',
					kind: 'background',
						toolName: 'files.write',
					status: 'running',
					sessionId: 'ses-00000000000000000000000000000001',
					startedAt: '2026-10-03T03:00:00Z',
					preview: '扫描中',
					output: '已移动 3 个文件',
				},
			],
			awaitingBackground: true,
			awaitingBackgroundCount: 1,
		});

		const card = container.querySelector('.tool-card');
		expect(card).toBeTruthy();
		expect(card?.textContent).toContain('执行中');
		expect(card?.textContent).toContain('后台任务运行中');
		expect(card?.textContent).toContain('files.write');
		expect(card?.textContent).toContain('扫描中');
		expect(card?.textContent).toContain('等待后台任务结果，完成后将自动继续');
		expect(container.querySelectorAll('.tool-run-wait-note')).toHaveLength(1);
		expect(container.querySelector('.tool-run-timeline-card')).toBeNull();
		expect(container.querySelector('.awaiting-bg-banner')).toBeNull();
		expect(container.querySelector('.activity-group')?.textContent).toContain(
			'等待后台任务结果',
		);
	});

	it('passes through the continue handler for a user-tail conversation', () => {
		render(SessionTimeline, {
			messages: [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: '继续处理' }],
			showContinueButton: true,
		});

		expect(screen.getByRole('button', { name: '继续生成' })).toBeTruthy();
		expect(document.querySelector('.continue-action')).toBeTruthy();
		expect(
			screen.getByRole('button', { name: '继续生成' }).classList.contains('md-btn--outlined'),
		).toBe(true);
		expect(screen.getByRole('button', { name: '继续生成' }).querySelector('svg')).toBeNull();
	});

	it('removes the continue handler while it is unavailable', () => {
		render(SessionTimeline, {
			messages: [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: '继续处理' }],
			showContinueButton: true,
			continueDisabled: true,
		});

		expect(screen.queryByRole('button', { name: '继续生成' })).toBeNull();
		expect(document.querySelector('.continue-action')).toBeNull();
	});

	it('keeps batch disclosure state when the step preamble is reconciled', async () => {
		const tools = [
			{
				id: 'step-ed4d1203d9eee442682361d4f077efd7',
				role: 'assistant' as const,
				content: 'first result',
				type: 'tool' as const,
				toolName: 'shell',
				stepNumber: 7,
				streaming: false,
			},
			{
				id: 'step-e23a75ffc10203f54edf15d337e0a880',
				role: 'assistant' as const,
				content: 'second result',
				type: 'tool' as const,
				toolName: 'files',
				stepNumber: 7,
				streaming: false,
			},
		];
		const { container, rerender } = render(SessionTimeline, { messages: tools });
		const headers = () =>
			Array.from(container.querySelectorAll('.md-collapsible-header')) as HTMLButtonElement[];

		await fireEvent.click(headers()[0]);
		await fireEvent.click(headers()[1]);
		expect(headers()[0].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');

		await rerender({
			messages: [
				{
					id: 'msg-eddbe8412b7a8205685b0a41d546042c',
					role: 'assistant' as const,
					content: '检查相关文件',
					type: 'thought' as const,
					stepNumber: 7,
					streaming: false,
				},
				...tools,
			],
		});

		expect(headers()[0].getAttribute('aria-expanded')).toBe('true');
		expect(headers()[1].getAttribute('aria-expanded')).toBe('true');
	});
});
