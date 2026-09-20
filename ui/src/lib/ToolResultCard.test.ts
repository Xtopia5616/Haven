import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ToolResultCard from './ToolResultCard.svelte';
import GlobalContextMenu from './GlobalContextMenu.svelte';
import { canRenderToolResult, parseToolResult } from './toolResultParsing.ts';
import { actionStore, toolOutputPreviewStore, upsertAction } from './stores.ts';
import { getToolResultRenderer } from './toolResultRenderers.ts';

const searchJson = (results: any[], extra: any = {}) =>
	JSON.stringify({ results, count: results.length, mode: 'filename', ...extra });

async function expandToolCard(container: HTMLElement) {
	const header = container.querySelector('.md-collapsible-header');
	if (header) await fireEvent.click(header);
}

describe('canRenderToolResult', () => {
	it('accepts search with a results array', () => {
		expect(canRenderToolResult('files', searchJson([{ path: 'a.rs' }]))).toBe(true);
	});
	it('accepts grouped system/haven results and independent aggregate tools', () => {
		expect(canRenderToolResult('system', JSON.stringify({ cpu: { usage_pct: 12 } }))).toBe(
			true,
		);
		expect(canRenderToolResult('system', JSON.stringify({ scope: 'process', processes: [{ pid: 1 }] }))).toBe(
			true,
		);
		expect(canRenderToolResult('system', JSON.stringify({ scope: 'window', windows: [{ title: 'x' }] }))).toBe(
			true,
		);
		expect(canRenderToolResult('haven', JSON.stringify({ operation: 'actions_list', status: 'running' }))).toBe(true);
		expect(canRenderToolResult('haven', JSON.stringify({ operation: 'schedule_list', scheduled_actions: [] }))).toBe(true);
		expect(canRenderToolResult('haven', JSON.stringify({ operation: 'schedule_set', id: 'r1', mode: 'notify' }))).toBe(
			true,
		);
		expect(canRenderToolResult('haven', JSON.stringify({ operation: 'schedule_cancel', cancelled: 'act-1' }))).toBe(true);
		expect(canRenderToolResult('memory', JSON.stringify({ operation: 'search', facts: [] }))).toBe(true);
		expect(canRenderToolResult('haven', JSON.stringify({ operation: 'tool_disable', name: 'files', enabled: true }))).toBe(true);
		expect(canRenderToolResult('system', JSON.stringify({ variables: [] }))).toBe(true);
		expect(canRenderToolResult('files', JSON.stringify({ written: true, path: 'x' }))).toBe(
			true,
		);
		expect(canRenderToolResult('http', JSON.stringify({ status: 200 }))).toBe(true);
		expect(canRenderToolResult('system', JSON.stringify({ scope: 'clipboard', content: 'hi' }))).toBe(true);
		expect(canRenderToolResult('system', JSON.stringify({ battery_percent: 80 }))).toBe(true);
		expect(
			canRenderToolResult(
				'system',
				JSON.stringify({ scope: 'process', operation: 'kill', killed: 42 }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'actions_cancel', action_id: 'act-1', cancelled: true }),
			),
		).toBe(true);
	});
	it('accepts shell text, notify text and any JSON observation', () => {
		expect(canRenderToolResult('shell', 'plain stdout text')).toBe(true);
		expect(canRenderToolResult('shell', JSON.stringify({ output: 'x' }))).toBe(true);
		expect(canRenderToolResult('notify', 'Notification sent: Build: done')).toBe(true);
		expect(
			canRenderToolResult(
				'load_mcp',
				JSON.stringify({ server_name: 'fs', status: 'loaded' }),
			),
		).toBe(true);
		expect(canRenderToolResult('media', JSON.stringify({ operation: 'play', played: true }))).toBe(true);
		expect(canRenderToolResult('media', JSON.stringify({ operation: 'volume_get', volume: 0.5 }))).toBe(true);
		expect(canRenderToolResult('media', JSON.stringify({ operation: 'inspect', asset_id: 'asset-1', media: {} }))).toBe(true);
		expect(canRenderToolResult('system', JSON.stringify({ scope: 'input', operation: 'click', clicked: [10, 20] }))).toBe(true);
		expect(canRenderToolResult('files', JSON.stringify({ nope: 1 }))).toBe(true);
	});
	it('accepts any non-empty text as a raw card', () => {
		expect(canRenderToolResult('files', '{not json[... truncated')).toBe(true);
		expect(canRenderToolResult('media', 'plain text')).toBe(true);
		expect(canRenderToolResult('notify', 'Some other text')).toBe(true);
	});
	it('keeps operation-scoped builtin results on their dedicated renderer path', () => {
		expect(
			parseToolResult('files', JSON.stringify({ operation: 'create_dir', created: true })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('system', JSON.stringify({ scope: 'info', networks: [] })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('system', JSON.stringify({ scope: 'process', operation: 'kill', killed: 42 })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('system', JSON.stringify({ scope: 'window', operation: 'screenshot', asset_id: 'asset-1' })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('media', JSON.stringify({ operation: 'describe', asset_id: 'asset-1', text: 'a screen' })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('haven', JSON.stringify({ operation: 'actions_cancel', action_id: 'act-1' })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('system', JSON.stringify({ scope: 'window', available: false, note: 'Windows only' })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('memory', JSON.stringify({ operation: 'search', facts: [] })),
		).toMatchObject({ kind: 'custom' });
		expect(
			parseToolResult('haven', JSON.stringify({ operation: 'tool_disable', name: 'files', enabled: true })),
		).toMatchObject({ kind: 'custom' });
	});
	it('routes independent operation views through their root renderer', () => {
		expect(
			parseToolResult(
				'files.read',
				JSON.stringify({ operation: 'read', path: 'a.rs', content: 'x' }),
			),
		).toMatchObject({ kind: 'custom' });
		expect(parseToolResult('files.search', searchJson([{ path: 'a.rs', line: 2 }]))).toMatchObject({
			kind: 'custom',
		});
		expect(
			parseToolResult('system.info', JSON.stringify({ scope: 'info', os: { name: 'Windows' } })),
		).toMatchObject({ kind: 'custom' });
	});
	it('rejects empty content', () => {
		expect(canRenderToolResult('', '')).toBe(false);
		expect(canRenderToolResult('files', '')).toBe(false);
	});
});

describe('operation view UI contract', () => {
	it('uses the backend renderer discriminator for operation results', () => {
		expect(getToolResultRenderer('custom', 'files.search', { results: [] }, 'files.search')).toBeTruthy();
	});
});

describe('parseToolResult', () => {
	it('returns null for empty content on non-shell tools', () => {
		expect(parseToolResult('files', '')).toBeNull();
	});
	it('keeps an empty shell card for streaming placeholders', () => {
		expect(parseToolResult('shell', '')).toEqual({ kind: 'shell', data: null });
	});
	it('classifies non-JSON text, arrays and primitives as raw', () => {
		expect(parseToolResult('files', 'plain text')).toEqual({ kind: 'raw', data: null });
		expect(parseToolResult('unknown_tool', JSON.stringify([1, 2]))).toEqual({
			kind: 'raw',
			data: [1, 2],
		});
		expect(parseToolResult('unknown_tool', '42')).toEqual({ kind: 'raw', data: 42 });
	});
	it('classifies a web_search tool return with results as custom', () => {
		const payload = JSON.stringify({
			label: '已联网搜索',
			queries: ['capital of France'],
			results: [
				{
					title: 'Paris — Wikipedia',
					url: 'https://en.wikipedia.org/wiki/Paris',
					snippet: 'Paris is the capital of France.',
				},
			],
		});
		expect(parseToolResult('web_search', payload)).toEqual({
			kind: 'custom',
			data: {
				label: '已联网搜索',
				queries: ['capital of France'],
				results: [
					{
						title: 'Paris — Wikipedia',
						url: 'https://en.wikipedia.org/wiki/Paris',
						snippet: 'Paris is the capital of France.',
					},
				],
			},
		});
		expect(canRenderToolResult('web_search', payload)).toBe(true);
	});
	it('treats a bare web_search status label as raw text', () => {
		expect(parseToolResult('web_search', '已联网搜索')).toEqual({
			kind: 'raw',
			data: null,
		});
	});
	it('classifies a query-only web_search return (no results) as custom', () => {
		const payload = JSON.stringify({ label: '已联网搜索', queries: ['foo'], results: [] });
		expect(parseToolResult('web_search', payload)?.kind).toBe('custom');
	});
	it('rejects a web_search payload without a label', () => {
		const payload = JSON.stringify({ queries: ['foo'], results: [] });
		expect(parseToolResult('web_search', payload)?.kind).toBe('generic');
	});
});

describe('ToolResultCard ask', () => {
	it('renders an ask card with question, options and waiting indicator', () => {
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '你想怎么做？',
			options: ['方案 A', '方案 B'],
			awaiting: true,
			messageId: 'ask-1',
		});
		expect(container.querySelector('.tool-card')).toBeTruthy();
		expect(screen.getByText('Haven 需要你的回答')).toBeTruthy();
		expect(screen.getByText('你想怎么做？')).toBeTruthy();
		expect(screen.queryByText('回答方式')).toBeNull();
		expect(screen.getByText('方案 A')).toBeTruthy();
		expect(screen.getByText('方案 B')).toBeTruthy();
	});

	it('hides options and waiting once answered', () => {
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '已问过',
			options: ['方案 A'],
			awaiting: false,
		});
		expect(container.querySelector('.tool-card')).toBeTruthy();
		expect(container.querySelector('.ask-waiting')).toBeNull();
		expect(screen.queryByText('方案 A')).toBeNull();
	});

	it('shows waiting copy when ask has no options', () => {
		render(ToolResultCard, {
			type: 'ask',
			content: '你怎么看？',
			options: [],
			awaiting: true,
			messageId: 'ask-free',
		});
		expect(screen.queryByText('回答方式')).toBeNull();
	});

	it('toggles option selection and notifies onAskSelectionChange without submitting', async () => {
		const onAskSelectionChange = vi.fn();
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '选择？',
			options: ['立即执行', '稍后'],
			awaiting: true,
			messageId: 'ask-42',
			onAskSelectionChange,
		});
		await fireEvent.click(screen.getByText('立即执行'));
		expect(onAskSelectionChange).toHaveBeenCalledWith('ask-42', ['立即执行']);
		expect(container.querySelector('.ask-option.selected')?.textContent).toBe('立即执行');
		await fireEvent.click(screen.getByText('稍后'));
		expect(onAskSelectionChange).toHaveBeenCalledWith('ask-42', ['立即执行', '稍后']);
		await fireEvent.click(screen.getByText('立即执行'));
		expect(onAskSelectionChange).toHaveBeenCalledWith('ask-42', ['稍后']);
	});

	it('offers an explicit submit action after selecting quick replies', async () => {
		const onAskSubmit = vi.fn();
		render(ToolResultCard, {
			type: 'ask',
			content: '选择？',
			options: ['立即执行'],
			awaiting: true,
			messageId: 'ask-submit',
			onAskSubmit,
		});

		const submit = screen.getByRole('button', { name: '提交回答' }) as HTMLButtonElement;
		expect(submit.disabled).toBe(true);
		await fireEvent.click(screen.getByText('立即执行'));
		expect(submit.disabled).toBe(false);
		await fireEvent.click(submit);
		expect(onAskSubmit).toHaveBeenCalledWith('ask-submit');
	});

	it('fires onIgnore with the message id', async () => {
		const onIgnore = vi.fn();
		render(ToolResultCard, {
			type: 'ask',
			content: '选择？',
			options: ['方案 A'],
			awaiting: true,
			messageId: 'ask-7',
			onIgnore,
		});
		expect(screen.getByRole('button', { name: '忽略' }).classList).toContain('md-btn--text');
		await fireEvent.click(screen.getByText('忽略'));
		expect(onIgnore).toHaveBeenCalledWith('ask-7');
	});

	it('shows the chosen answer and hides buttons once resolved', () => {
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '选哪个？',
			options: ['方案 A'],
			awaiting: false,
			resolved: { answer: '方案 A' },
		});
		expect(screen.getByText('已选择：方案 A')).toBeTruthy();
		expect(container.querySelector('.ask-option')).toBeNull();
		expect(container.querySelector('.ask-ignore')).toBeNull();
		expect(container.querySelector('.ask-waiting')).toBeNull();
	});

	it('shows 已忽略 once the question is ignored', () => {
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '选哪个？',
			options: ['方案 A'],
			awaiting: false,
			resolved: { ignored: true },
		});
		expect(screen.getByText('已忽略')).toBeTruthy();
		expect(container.querySelector('.ask-ignore')).toBeNull();
	});
});

describe('ToolResultCard outcomes', () => {
	it('surfaces unknown side-effect state instead of implying a safe retry', () => {
		render(ToolResultCard, {
			type: 'tool',
			toolName: 'messaging',
			content: 'request timed out',
			outcome: 'unknown',
		});
		expect(screen.getByText('结果未知，可能已执行')).toBeTruthy();
		expect(screen.getByTitle('该操作可能已经产生副作用，禁止自动重试')).toBeTruthy();
	});
});

describe('ToolResultCard shell / notify / generic', () => {
	it('renders plain shell output in a terminal card', async () => {
		const { container } = render(ToolResultCard, { toolName: 'shell', content: 'Hello from cmd' });
		await expandToolCard(container);
		expect(screen.getByText('终端输出')).toBeTruthy();
		expect(screen.getByText('Hello from cmd')).toBeTruthy();
	});

	it('renders JSON shell output with the truncated note', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: JSON.stringify({ output: 'line1\nline2', truncated: true }),
		});
		await expandToolCard(container);
		expect(container.querySelector('.content-preview')!.textContent).toBe('line1\nline2');
		expect(screen.getByText('输出过长已截断')).toBeTruthy();
	});

	it('renders the exit code for a completed background shell action', async () => {
		upsertAction({
			id: 'act-shell-1',
			kind: 'background',
			status: 'completed',
			output: 'command output',
			exitCode: 0,
		});
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			actionId: 'act-shell-1',
			content: '',
		});
		await expandToolCard(container);
		expect(container.querySelector('.tool-card-count')?.textContent).toContain('后台任务已完成');
		expect(screen.getByText('退出码 0')).toBeTruthy();
		expect(screen.getByText('command output')).toBeTruthy();
	});

	it('renders a notification card with title and body', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'notify',
			content: 'Notification sent: 构建完成: 全部测试通过',
		});
		await expandToolCard(container);
		expect(screen.getByText('通知')).toBeTruthy();
		expect(screen.getByText('构建完成')).toBeTruthy();
		expect(screen.getByText('全部测试通过')).toBeTruthy();
	});

	it('renders a generic JSON tree card for tools without a custom shape', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'load_mcp',
			content: JSON.stringify({ server_name: 'filesystem', status: 'loaded' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('加载 MCP')).toBeTruthy();
		expect(container.querySelector('.jv-view')).toBeTruthy();
		expect(screen.getByText('"server_name"')).toBeTruthy();
		expect(screen.getByText('"filesystem"')).toBeTruthy();
	});
});

afterEach(() => {
	actionStore.set({});
	toolOutputPreviewStore.set({});
});

describe('ToolResultCard raw', () => {
	it('renders plain text output in a raw card with the tool label', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: 'some plain text',
		});
		await expandToolCard(container);
		expect(screen.getByText('媒体')).toBeTruthy();
		expect(container.querySelector('.content-preview')!.textContent).toContain(
			'some plain text',
		);
	});

	it('pretty-prints JSON array observations', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'unknown_tool',
			content: JSON.stringify([1, 2, { a: 'b' }]),
		});
		await expandToolCard(container);
		expect(container.querySelector('.content-preview')!.textContent).toContain('"a"');
		expect(container.querySelector('.content-preview')!.textContent).toContain('"b"');
	});
});

describe('ToolResultCard usage', () => {
	it('shows only the tool-local token chip in the dropdown header', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			toolArgs: { command: 'dir' },
			content: 'ok',
		});

		const chips = container.querySelectorAll('.usage-chip');
		expect(chips).toHaveLength(1);
		expect(chips[0].textContent?.trim()).not.toBe('1.23K tokens');
		expect(chips[0].getAttribute('title')).toContain('调用参数');
		expect(chips[0].getAttribute('title')).toContain('返回结果');
		expect(chips[0].getAttribute('title')).not.toContain('模型');
	});

	it('shows an independent estimated data-token chip for each tool', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			toolArgs: { command: 'dir' },
			content: 'file.txt',
		});

		const chip = container.querySelector('.usage-chip') as HTMLElement;
		expect(chip).toBeTruthy();
		expect(chip.textContent?.trim()).toMatch(/tokens$/);
		expect(chip.getAttribute('title')).toContain('调用参数');
		expect(chip.getAttribute('title')).toContain('返回结果');
		expect(chip.getAttribute('title')).toContain('合计');
		expect(chip.getAttribute('title')).not.toContain('估算');
	});
});

describe('ToolResultCard source + args', () => {
	it('shows a builtin source badge next to the Chinese label', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: 'ok',
		});
		const badge = container.querySelector('.tool-source') as HTMLElement;
		expect(badge).toBeTruthy();
		expect(badge.getAttribute('data-source')).toBe('builtin');
		expect(badge.textContent).toBe('内置');
		expect(screen.getByText('终端输出')).toBeTruthy();
	});

	it('labels MCP tools with an MCP badge and stripped name', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'mcp__filesystem__read',
			content: JSON.stringify({ ok: true }),
			streaming: true,
			toolArgs: { path: 'a.rs' },
		});
		const badge = container.querySelector('.tool-source') as HTMLElement;
		expect(badge.getAttribute('data-source')).toBe('mcp');
		expect(badge.textContent).toBe('MCP');
		expect(container.querySelector('.tool-card-icon svg')).toBeTruthy();
		expect(screen.getByText('filesystem__read')).toBeTruthy();
		expect(screen.getByText('调用参数')).toBeTruthy();
		expect(screen.getByText('"path"')).toBeTruthy();
		expect(screen.getByText('"a.rs"')).toBeTruthy();
	});

	it('labels Skill tools and accepts resume action_input JSON strings', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'skill__weather',
			content: JSON.stringify({ temp: 20 }),
			toolArgs: '{"city":"Shanghai"}',
		});
		await expandToolCard(container);
		const badge = container.querySelector('.tool-source') as HTMLElement;
		expect(badge.getAttribute('data-source')).toBe('skill');
		expect(screen.getByText('weather')).toBeTruthy();
		expect(screen.getByText('调用参数')).toBeTruthy();
		expect(screen.getByText('"city"')).toBeTruthy();
		expect(screen.getByText('"Shanghai"')).toBeTruthy();
	});

	it('labels MCP activation cards while Skills use direct adapters', () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'load_mcp',
			content: JSON.stringify({ loaded: true }),
		});
		let badge = container.querySelector('.tool-source') as HTMLElement;
		expect(badge.getAttribute('data-source')).toBe('mcp');
		expect(badge.textContent).toBe('MCP');
		expect(container.querySelector('.tool-card-icon svg')).toBeTruthy();

		rerender({
			toolName: 'skill__weather',
			content: JSON.stringify({ loaded: true }),
		});
		badge = container.querySelector('.tool-source') as HTMLElement;
		expect(badge.getAttribute('data-source')).toBe('skill');
		expect(badge.textContent).toBe('Skill');
		expect(container.querySelector('.tool-card-icon svg')).toBeTruthy();
	});

	it('does not mount the args JsonView while the card is collapsed', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: 'ok',
			toolArgs: { command: 'echo hi' },
		});
		expect(container.querySelector('.tool-args')).toBeNull();
		expect(container.querySelector('.jv-view')).toBeNull();
	});
});

describe('ToolResultCard empty in-progress', () => {
	it('shows status, parameters and output sections for every call', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: JSON.stringify({ output: 'result' }),
			streaming: true,
			toolArgs: { command: 'echo hi', silent: true },
		});

		expect(container.querySelector('.tool-card')).toBeTruthy();
		expect(screen.getByText('终端输出')).toBeTruthy();
		expect(screen.getByText('执行中')).toBeTruthy();
		expect(screen.getByText('执行状态')).toBeTruthy();
		expect(screen.getByText('调用参数')).toBeTruthy();
		expect(screen.getByText('输出结果')).toBeTruthy();
		expect(screen.getByText('"command"')).toBeTruthy();
		expect(screen.getByText('"echo hi"')).toBeTruthy();
		expect(screen.getByText('result')).toBeTruthy();
	});

	it('shows the deterministic intent fallback when the model emitted no preamble', () => {
		render(ToolResultCard, {
			toolName: 'shell',
			content: '',
			streaming: true,
			showFallbackIntent: true,
		});
		expect(screen.getByText('调用工具')).toBeTruthy();
	});

	it('renders a collapsed card for an empty completed call', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: '',
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header).toBeTruthy();
		expect(header.getAttribute('aria-expanded')).toBe('false');
		expect(screen.getByText('文件与搜索')).toBeTruthy();
		expect(screen.queryByText('（无参数）')).toBeNull();
		expect(screen.queryByText('（无输出）')).toBeNull();
	});

	it('expands and shows a waiting placeholder while streaming with no content', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: '',
			streaming: true,
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header.getAttribute('aria-expanded')).toBe('true');
		expect(screen.getByText('等待输出…')).toBeTruthy();
	});
});

describe('ToolResultCard collapsible', () => {
	it('collapses the details once the observation is final', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header).toBeTruthy();
		expect(header.getAttribute('aria-expanded')).toBe('false');
		expect(screen.queryByText('收起详情')).toBeNull();
		expect(screen.queryByText('查看详情')).toBeNull();
	});

	it('collapses the details as streaming ends', async () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
			streaming: true,
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header.getAttribute('aria-expanded')).toBe('true');
		await rerender({ streaming: false });
		expect(header.getAttribute('aria-expanded')).toBe('false');
	});

	it('toggles open when the header is clicked and keeps a manual expand', async () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header.getAttribute('aria-expanded')).toBe('false');
		await fireEvent.click(header);
		expect(header.getAttribute('aria-expanded')).toBe('true');
		await rerender({ content: searchJson([{ path: 'b.rs' }]) });
		expect(header.getAttribute('aria-expanded')).toBe('true');
	});

	it('reopens when streaming starts after completion', async () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header.getAttribute('aria-expanded')).toBe('false');
		await rerender({ streaming: true, content: '' });
		expect(header.getAttribute('aria-expanded')).toBe('true');
	});

	it('does not reopen after a transient live-preview gap', async () => {
		const messageId = 'step-preview-gap';
		toolOutputPreviewStore.set({ [messageId]: 'first chunk' });
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'shell',
			messageId,
			streaming: true,
			content: '',
		});
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;

		await fireEvent.click(header);
		expect(header.getAttribute('aria-expanded')).toBe('false');
		await rerender({ streaming: false });
		toolOutputPreviewStore.set({});
		await tick();
		toolOutputPreviewStore.set({ [messageId]: 'second chunk' });
		await tick();

		expect(header.getAttribute('aria-expanded')).toBe('false');
	});
});

describe('ToolResultCard files', () => {
	it('renders create_dir with the file-specific result UI', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({ operation: 'create_dir', created: true, path: 'D:\\tmp\\reports' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已创建目录')).toBeTruthy();
		expect(screen.getByText('D:\\tmp\\reports')).toBeTruthy();
	});

	it('does not route a removed file_search alias to the files renderer', () => {
		expect(parseToolResult('file_search', searchJson([{ path: 'D:\\tmp\\match.rs' }]))).toMatchObject({
			kind: 'generic',
		});
	});

	it('renders a filename-mode search card with paths and count', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'D:\\workspace\\a.rs' }, { path: 'D:\\workspace\\b.rs' }]),
		});
		await expandToolCard(container);
		expect(screen.getByText('文件与搜索')).toBeTruthy();
		expect(screen.getByText('2 个结果 · 文件名')).toBeTruthy();
		expect(screen.getByText('D:\\workspace\\a.rs')).toBeTruthy();
		expect(screen.getByText('D:\\workspace\\b.rs')).toBeTruthy();
	});

	it('renders line numbers and snippets in content mode', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				results: [{ path: 'lib.rs', line: 42, snippet: 'fn main() {}' }],
				count: 1,
				mode: 'content',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('1 个结果 · 全文')).toBeTruthy();
		expect(screen.getByText('L42')).toBeTruthy();
		expect(screen.getByText('fn main() {}')).toBeTruthy();
	});

	it('renders file search results in bounded pages', async () => {
		const results = Array.from({ length: 250 }, (_, index) => ({
			path: `D:\\workspace\\match-${index}.rs`,
		}));
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson(results),
		});
		await expandToolCard(container);

		expect(container.querySelectorAll('.search-row')).toHaveLength(100);
		const moreButton = screen.getByRole('button', { name: '显示更多（剩余 150 条）' });
		expect(moreButton).toBeTruthy();

		await fireEvent.click(moreButton);
		expect(container.querySelectorAll('.search-row')).toHaveLength(200);
		expect(screen.getByRole('button', { name: '显示更多（剩余 50 条）' })).toBeTruthy();
	});

	it('renders the truncated hint when present', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a' }], { hint: 'Results hit the max_results cap.' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('Results hit the max_results cap.')).toBeTruthy();
	});
});

describe('ToolResultCard system', () => {
	it('renders network category results instead of generic JSON', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'info',
				networks: [{ name: 'Wi-Fi', state: 'up', ips: ['192.168.1.2'] }],
				count: 1,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('1 个网络接口')).toBeTruthy();
		expect(screen.getByText('Wi-Fi')).toBeTruthy();
		expect(screen.getByText('192.168.1.2')).toBeTruthy();
	});

	it('renders cpu/memory meters and os info', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				os: { name: 'Windows 11', hostname: 'DESKTOP-X', uptime_secs: 90000 },
				cpu: { brand: 'Ryzen', cores: 8, logical_cpus: 16, usage_pct: 25.5 },
				memory: { total_bytes: 16 * 1024 ** 3, used_bytes: 8 * 1024 ** 3 },
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('系统与桌面')).toBeTruthy();
		expect(screen.getByText('Windows 11')).toBeTruthy();
		expect(screen.getByText('DESKTOP-X')).toBeTruthy();
		expect(screen.getByText('25.5%')).toBeTruthy();
		expect(screen.getByText('8.0 GB / 16.0 GB')).toBeTruthy();
		expect(screen.getByText('8 核 / 16 线程')).toBeTruthy();
		expect(container.querySelectorAll('.meter-fill').length).toBe(2);
	});

	it('renders power status and no-battery state', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'power',
				ac_power: 'offline',
				battery_percent: null,
				battery_present: false,
				battery_status: 'unknown',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('使用电池')).toBeTruthy();
		expect(screen.getByText('未检测到电池')).toBeTruthy();
	});

	it('routes a process child through the system aggregate renderer', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ scope: 'process', operation: 'kill', killed: 42 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已终止')).toBeTruthy();
		expect(screen.getByText('PID 42')).toBeTruthy();
	});
});

describe('ToolResultCard haven aggregate', () => {
	it('routes prefixed action operations to the action renderer', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({
				operation: 'actions_cancel',
				action_id: 'act-2',
				cancelled: true,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('act-2')).toBeTruthy();
	});
});

describe('ToolResultCard process', () => {
	it('renders a kill result with the process-specific action UI', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ scope: 'process', operation: 'kill', killed: 42 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已终止')).toBeTruthy();
		expect(screen.getByText('PID 42')).toBeTruthy();
	});

	it('renders a process table with pid, cpu and memory', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'process',
				processes: [
					{ pid: 100, name: 'chrome.exe', cpu: 3.5, memory: 500 * 1024 * 1024 },
					{ pid: 200, name: 'explorer.exe', cpu: 0.2, memory: 200 * 1024 * 1024 },
				],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('2 个进程')).toBeTruthy();
		expect(screen.getByText('chrome.exe')).toBeTruthy();
		expect(screen.getByText('explorer.exe')).toBeTruthy();
		expect(screen.getByText('500 MB')).toBeTruthy();
	});

	it('renders a status badge column with mapped labels', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'process',
				processes: [
					{ pid: 1, name: 'a.exe', status: 'Run' },
					{ pid: 2, name: 'b.exe', status: 'Sleep' },
					{ pid: 3, name: 'c.exe', status: 'Zombie' },
				],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('运行中')).toBeTruthy();
		expect(screen.getByText('休眠')).toBeTruthy();
		expect(screen.getByText('僵尸')).toBeTruthy();
	});

	it('filters processes by name', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'process',
				processes: [
					{ pid: 1, name: 'chrome.exe' },
					{ pid: 2, name: 'explorer.exe' },
				],
			}),
		});
		await expandToolCard(container);
		await fireEvent.input(screen.getByPlaceholderText('筛选进程...'), {
			target: { value: 'chrome' },
		});
		expect(screen.getByText('1 / 2 个进程')).toBeTruthy();
		expect(screen.getByText('chrome.exe')).toBeTruthy();
		expect(screen.queryByText('explorer.exe')).toBeNull();
	});

	it('collapses beyond 50 processes with a show-all toggle', async () => {
		const processes = Array.from({ length: 60 }, (_, i) => ({ pid: i + 1, name: `p${i}.exe` }));
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ scope: 'process', processes }),
		});
		await expandToolCard(container);
		expect(screen.getByText('60 个进程')).toBeTruthy();
		expect(screen.queryByText('p59.exe')).toBeNull();
		await fireEvent.click(screen.getByText('显示全部 60 个进程'));
		expect(screen.getByText('p59.exe')).toBeTruthy();
		await fireEvent.click(screen.getByText('收起'));
		expect(screen.queryByText('p59.exe')).toBeNull();
	});
});

describe('ToolResultCard actions', () => {
	it('renders the action id with a completed badge', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({ operation: 'actions_list', action_id: 'act-1', status: 'completed', exit_code: 0 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('act-1')).toBeTruthy();
		expect(screen.getByText('已完成')).toBeTruthy();
		 expect(screen.getByText('退出码 0')).toBeTruthy();
	});

	it('renders cancel results with an explicit action status', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({ operation: 'actions_cancel', action_id: 'act-2', cancelled: true }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('act-2')).toBeTruthy();
	});
});

describe('ToolResultCard window', () => {
	it('renders screenshot results with a managed asset reference and dimensions', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'window',
				operation: 'screenshot',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
				width: 1920,
				height: 1080,
				format: 'png',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('截图已生成')).toBeTruthy();
		expect(screen.getByText('asset-0123456789abcdef0123456789abcdef')).toBeTruthy();
		expect(screen.getByText('1920×1080 · PNG')).toBeTruthy();
	});

	it('renders OCR through the canonical media result shape', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				scope: 'window',
				operation: 'ocr',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
				media: {
					asset_id: 'asset-0123456789abcdef0123456789abcdef',
					representation: 'ocr_text',
					content: '窗口中的文字',
				},
				representation: 'ocr_text',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('ocr')).toBeTruthy();
		expect(screen.getByText('asset-0123456789abcdef0123456789abcdef')).toBeTruthy();
		expect(screen.getByText('窗口中的文字')).toBeTruthy();
	});
});

describe('ToolResultCard media recording', () => {
	it('shows capability-unavailable recording results', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: JSON.stringify({
				operation: 'record',
				available: false,
				capability: 'record',
				reason_code: 'record_unavailable',
				reason: '麦克风采集未配置',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('麦克风采集未配置')).toBeTruthy();
	});

	it('shows capture errors while retaining the recorded asset', async () => {
		const assetId = 'asset-0123456789abcdef0123456789abcdef';
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: JSON.stringify({
				success: false,
				operation: 'record',
				asset_id: assetId,
				capture_error: true,
				error: '麦克风没有检测到声音',
				media: { asset_id: assetId, representation: 'managed_file_ref' },
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText(assetId)).toBeTruthy();
		expect(screen.getByText('麦克风没有检测到声音')).toBeTruthy();
	});
});

describe('ToolResultCard files', () => {
	it('renders managed binary media through the canonical media card', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				operation: 'read',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
				media: {
					asset_id: 'asset-0123456789abcdef0123456789abcdef',
					filename: 'diagram.png',
					content: 'A flow diagram with three nodes.',
					available_representations: ['managed_file_ref', 'image_description'],
					recommended_next: 'media.describe',
				},
				representation: 'image_description',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('asset-0123456789abcdef0123456789abcdef')).toBeTruthy();
		expect(screen.getByText('diagram.png')).toBeTruthy();
		expect(screen.getByText('A flow diagram with three nodes.')).toBeTruthy();
		expect(screen.getByText('可用表示：managed_file_ref、image_description')).toBeTruthy();
		expect(screen.getByText('建议下一步：media.describe')).toBeTruthy();
	});

	it('renders image analysis results instead of a blank read state', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				operation: 'read',
				image: true,
				path: 'C:\\tmp\\diagram.png',
				description: 'A flow diagram with three nodes.',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('图像读取完成')).toBeTruthy();
		expect(screen.getByText('A flow diagram with three nodes.')).toBeTruthy();
	});

	it('renders write / delete results', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({ written: true, path: 'C:\\tmp\\out.txt' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已写入')).toBeTruthy();
		expect(screen.getByText('C:\\tmp\\out.txt')).toBeTruthy();
	});

	it('renders directory listing entries', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({ entries: ['a.txt', 'b.rs'], count: 2 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('2 项')).toBeTruthy();
		expect(screen.getByText('a.txt')).toBeTruthy();
		expect(screen.getByText('b.rs')).toBeTruthy();
	});
});

describe('ToolResultCard http', () => {
	it('renders status badge and body preview', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'http',
			content: JSON.stringify({ status: 200, body: '{"ok":true}', truncated: false }),
		});
		await expandToolCard(container);
		expect(screen.getByText('200')).toBeTruthy();
		expect(screen.getByText('{"ok":true}')).toBeTruthy();
	});

	it('marks non-2xx status as failed', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'http',
			content: JSON.stringify({ status: 404, body: '' }),
		});
		await expandToolCard(container);
		expect(container.querySelector('.status-failed')).toBeTruthy();
	});
	it('renders a single scheduled action result with id, mode and fires_at', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({
				operation: 'schedule_set',
				id: 'r42',
				mode: 'tool',
				fires_at: '2026-08-05T09:00:00+08:00',
				wakes_session: true,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('#r42')).toBeTruthy();
		expect(screen.getByText('调用工具')).toBeTruthy();
		expect(screen.getByText('触发时间 2026-08-05T09:00:00+08:00')).toBeTruthy();
	});

	it('renders a schedule cancellation as a dedicated result', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({ operation: 'schedule_cancel', cancelled: 'act-42' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('#act-42')).toBeTruthy();
	});
});

describe('ToolResultCard memory', () => {
	it('renders fact search results as readable triples', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'memory',
			content: JSON.stringify({
				operation: 'search',
				facts: [{ subject: 'user', predicate: '喜欢', object: 'Rust', confidence: 0.92 }],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('1 条记忆事实')).toBeTruthy();
		expect(screen.getByText('喜欢')).toBeTruthy();
		expect(screen.getByText('Rust')).toBeTruthy();
		expect(screen.getByText('置信度 0.92')).toBeTruthy();
	});

	it('renders recall hits and empty states', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'memory',
			content: JSON.stringify({ operation: 'recall', hits: [], mode: 'keyword' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('0 条召回结果 · keyword')).toBeTruthy();
		expect(screen.getByText('没有找到相关记忆')).toBeTruthy();
	});
});

describe('ToolResultCard admin capabilities', () => {
	it('renders tool toggles as a compact status result', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'haven',
			content: JSON.stringify({ operation: 'tool_disable', name: 'files', enabled: false, saved: true }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已停用')).toBeTruthy();
		expect(screen.getByText('files')).toBeTruthy();
	});
});

describe('ToolResultCard media audio operations and input', () => {
	it('renders volume results with a human-readable percentage', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: JSON.stringify({ operation: 'volume_get', volume: 0.5 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('当前音量')).toBeTruthy();
		expect(screen.getByText('50%')).toBeTruthy();
	});

	it('renders a speak result without exposing the synthesized text', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: JSON.stringify({ operation: 'speak', spoken: true, characters: 12 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('朗读完成')).toBeTruthy();
		expect(screen.getByText('12 字')).toBeTruthy();
	});

	it('renders recorded media identity and next representation', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: JSON.stringify({
				operation: 'record',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
				media: {
					asset_id: 'asset-0123456789abcdef0123456789abcdef',
					representation: 'transcript',
					available_representations: ['managed_file_ref', 'transcript'],
					recommended_next: null,
				},
				transcript: 'hello',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('资产：asset-0123456789abcdef0123456789abcdef')).toBeTruthy();
		expect(screen.getByText('表示：transcript')).toBeTruthy();
		expect(screen.getByText('可用表示：managed_file_ref、transcript')).toBeTruthy();
	});

	it('renders input results with the action and coordinates', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ scope: 'input', operation: 'click', clicked: [10, 20], button: 'left' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已点击')).toBeTruthy();
		expect(screen.getByText('10, 20')).toBeTruthy();
	});
});

describe('ToolResultCard system env', () => {
	it('renders a variables list', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ variables: [{ name: 'PATH', value: 'C:\\bin' }], count: 1 }),
		});
		await expandToolCard(container);
		expect(screen.getByText('1 个变量')).toBeTruthy();
		expect(screen.getByText('PATH')).toBeTruthy();
		expect(screen.getByText('C:\\bin')).toBeTruthy();
	});

	it('filters variables by name and value', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				variables: [
					{ name: 'PATH', value: 'C:\\bin' },
					{ name: 'HOME', value: 'C:\\Users\\me' },
				],
			}),
		});
		await expandToolCard(container);
		await fireEvent.input(screen.getByPlaceholderText('筛选变量...'), {
			target: { value: 'path' },
		});
		expect(screen.getByText('1 / 2 个变量')).toBeTruthy();
		expect(screen.getByText('PATH')).toBeTruthy();
		expect(screen.queryByText('HOME')).toBeNull();
		await fireEvent.input(screen.getByPlaceholderText('筛选变量...'), {
			target: { value: 'Users' },
		});
		expect(screen.getByText('1 / 2 个变量')).toBeTruthy();
		expect(screen.getByText('HOME')).toBeTruthy();
		expect(screen.queryByText('PATH')).toBeNull();
	});

	it('copies an env value via the copy button', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ variables: [{ name: 'API_KEY', value: 'secret-value' }] }),
		});
		await expandToolCard(container);
		await fireEvent.click(screen.getByTitle('复制值'));
		expect(writeText).toHaveBeenCalledWith('secret-value');
	});
});

describe('ToolResultCard context menu', () => {
	it('copies displayed shell output from the right-click menu', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(GlobalContextMenu);
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: JSON.stringify({ output: 'hello stdout' }),
		});
		await fireEvent.contextMenu(container.querySelector('.tool-card')!);
		expect(screen.getByText('复制输出')).toBeTruthy();
		await fireEvent.click(screen.getByText('复制输出'));
		expect(writeText).toHaveBeenCalledWith('hello stdout');
	});

	it('copies the ask question from the right-click menu', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		render(GlobalContextMenu);
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '继续吗？',
			options: ['是'],
			awaiting: true,
		});
		await fireEvent.contextMenu(container.querySelector('.tool-card')!);
		expect(screen.getByText('复制问题')).toBeTruthy();
		await fireEvent.click(screen.getByText('复制问题'));
		expect(writeText).toHaveBeenCalledWith('继续吗？');
	});
});
