import { fireEvent, render, screen } from '@testing-library/svelte';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
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

	it('renders the message timeline immediately when the first message arrives', () => {
		render(SessionTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '你好', type: 'user' }],
		});

		expect(document.querySelector('.message-list')).toBeTruthy();
		expect(document.querySelector('.bubble.user')?.textContent).toContain('你好');
	});

	it('renders waiting background progress inside its source tool card', () => {
		const { container } = render(SessionTimeline, {
			messages: [
				{
					id: 'step-background',
					role: 'assistant',
					content: '{"execution_mode":"background","tool_run_id":"toolrun-bg"}',
					type: 'tool',
					toolName: 'shell',
					sourceToolRunId: 'toolrun-bg',
					stepNumber: 2,
					streaming: false,
				},
			],
			sessionToolRuns: [
				{
					id: 'toolrun-bg',
					kind: 'background',
					status: 'running',
					sessionId: 'ses-1',
					startedAt: '2026-10-03T03:00:00Z',
					command: '整理下载目录',
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
		expect(card?.textContent).toContain('整理下载目录');
		expect(card?.textContent).toContain('扫描中');
		expect(card?.textContent).toContain('等待后台任务结果，完成后将自动继续');
		expect(container.querySelectorAll('.tool-run-wait-note')).toHaveLength(1);
		expect(container.querySelector('.tool-run-timeline-card')).toBeNull();
		expect(container.querySelector('.awaiting-bg-banner')).toBeNull();
		expect(container.querySelector('.activity-group')?.textContent).toContain('等待后台任务结果');
	});

	it('uses the shared ToolRun card for scheduled details when the transcript is empty', () => {
		const { container } = render(SessionTimeline, {
			messages: [],
			sessionToolRuns: [
				{
					id: 'toolrun-scheduled',
					kind: 'scheduled',
					status: 'waiting',
					sessionId: 'ses-1',
					dueAt: '2026-10-03T04:00:00Z',
					title: '稍后提醒',
					body: '整理会议记录',
					mode: 'continue',
				},
			],
		});

		const card = container.querySelector('.tool-run-timeline-card');
		expect(card).toBeTruthy();
		expect(card?.textContent).toContain('定时任务');
		expect(card?.textContent).toContain('稍后提醒');
		expect(card?.textContent).toContain('整理会议记录');
		expect(card?.textContent).toContain('继续会话');
		expect(card?.textContent).toContain('计划触发');
		expect(container.querySelector('.welcome')).toBeNull();
	});

	it('passes through the continue handler for a user-tail conversation', () => {
		render(SessionTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '继续处理', type: 'user' }],
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
			messages: [{ id: 'msg-1', role: 'user', content: '继续处理', type: 'user' }],
			showContinueButton: true,
			continueDisabled: true,
		});

		expect(screen.queryByRole('button', { name: '继续生成' })).toBeNull();
		expect(document.querySelector('.continue-action')).toBeNull();
	});

	it('keeps batch disclosure state when the step preamble is reconciled', async () => {
		const tools = [
			{
				id: 'step-a',
				role: 'assistant',
				content: 'first result',
				type: 'tool',
				toolName: 'shell',
				stepNumber: 7,
				streaming: false,
			},
			{
				id: 'step-b',
				role: 'assistant',
				content: 'second result',
				type: 'tool',
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
					id: 'msg-preamble',
					role: 'assistant',
					content: '检查相关文件',
					type: 'thought',
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
