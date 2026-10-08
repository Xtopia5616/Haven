import { describe, it, expect, vi, afterEach } from 'vitest';
import { render as testingLibraryRender, screen, fireEvent } from '@testing-library/svelte';
import { tick } from 'svelte';
import ToolResultCard from './ToolResultCard.svelte';
import GlobalContextMenu from './GlobalContextMenu.svelte';
import { canRenderToolResult, parseToolResult } from './toolResultParsing.ts';
import { toolRunStore, upsertToolRun } from './toolRunStore.ts';
import {
	clearToolOutputPreview,
	clearToolOutputPreviewsForSession,
	setToolOutputPreview,
} from './toolOutputPreviewStore.ts';
import { getToolResultRenderer } from './toolResultRenderers.ts';
import ToolFileResult from './ToolFileResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import ToolHttpResult from './ToolHttpResult.svelte';
import ToolInputResult from './ToolInputResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import ToolMediaResult from './ToolMediaResult.svelte';
import ToolMemoryResult from './ToolMemoryResult.svelte';
import ToolShellResult from './ToolShellResult.svelte';
import ToolSystemResult from './ToolSystemResult.svelte';
import ToolRunsResult from './ToolRunsResult.svelte';
import ToolWebSearchResult from './ToolWebSearchResult.svelte';
import ToolWindowResult from './ToolWindowResult.svelte';
import ToolScheduleResult from './ToolScheduleResult.svelte';

const searchJson = (results: any[], extra: any = {}) =>
	JSON.stringify({ results, count: results.length, mode: 'filename', ...extra });

function render(component: any, props: Record<string, any> = {}) {
	if (component !== ToolResultCard) return testingLibraryRender(component, props);
	const renderers: Record<string, string> = {
		files: 'files',
		http: 'http',
		load_mcp: 'load_mcp',
		mcp__filesystem__read: 'filesystem',
		media: 'media',
		memory: 'memory',
		messaging: 'messaging',
		notify: 'notify',
		shell: 'shell',
		skill__weather: 'weather',
		system: 'system',
		window: 'window',
	};
	const renderer = props.renderer ?? renderers[props.toolName];
	return testingLibraryRender(component, { ...props, ...(renderer ? { renderer } : {}) });
}

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
		expect(
			canRenderToolResult(
				'system',
				JSON.stringify({ scope: 'process', processes: [{ pid: 1 }] }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'system',
				JSON.stringify({ scope: 'window', windows: [{ title: 'x' }] }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'tool_runs_list', status: 'running' }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'schedule_list', scheduled_tool_runs: [] }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'schedule_set', tool_run_id: 'r1', mode: 'notify' }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'schedule_cancel', tool_run_id: 'toolrun-1' }),
			),
		).toBe(true);
		expect(
			canRenderToolResult('memory', JSON.stringify({ operation: 'search', facts: [] })),
		).toBe(true);
		expect(
			canRenderToolResult(
				'haven',
				JSON.stringify({ operation: 'tool_disable', name: 'files', enabled: true }),
			),
		).toBe(true);
		expect(canRenderToolResult('system', JSON.stringify({ variables: [] }))).toBe(true);
		expect(canRenderToolResult('files', JSON.stringify({ written: true, path: 'x' }))).toBe(
			true,
		);
		expect(canRenderToolResult('http', JSON.stringify({ status: 200 }))).toBe(true);
		expect(
			canRenderToolResult('system', JSON.stringify({ scope: 'clipboard', content: 'hi' })),
		).toBe(true);
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
				JSON.stringify({
					operation: 'tool_runs_cancel',
					tool_run_id: 'toolrun-1',
					cancelled: true,
				}),
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
		expect(
			canRenderToolResult('media', JSON.stringify({ operation: 'play', played: true })),
		).toBe(true);
		expect(
			canRenderToolResult('media', JSON.stringify({ operation: 'volume_get', volume: 0.5 })),
		).toBe(true);
		expect(
			canRenderToolResult(
				'media',
				JSON.stringify({ operation: 'inspect', asset_id: 'asset-1', media: {} }),
			),
		).toBe(true);
		expect(
			canRenderToolResult(
				'system',
				JSON.stringify({ scope: 'input', operation: 'click', clicked: [10, 20] }),
			),
		).toBe(true);
		expect(canRenderToolResult('files', JSON.stringify({ nope: 1 }))).toBe(true);
	});
	it('accepts any non-empty text as a raw card', () => {
		expect(canRenderToolResult('files', '{not json[... truncated')).toBe(true);
		expect(canRenderToolResult('media', 'plain text')).toBe(true);
		expect(canRenderToolResult('notify', 'Some other text')).toBe(true);
	});
	it('uses the renderer contract instead of inferring a renderer from result shape', () => {
		const result = JSON.stringify({ operation: 'create_dir', created: true });
		expect(parseToolResult('files.create_dir', result, 'files')).toMatchObject({
			kind: 'custom',
		});
		expect(parseToolResult('files.create_dir', result)).toMatchObject({ kind: 'generic' });
	});
	it('rejects empty content', () => {
		expect(canRenderToolResult('', '')).toBe(false);
		expect(canRenderToolResult('files', '')).toBe(false);
	});
});

describe('operation view UI contract', () => {
	it('uses the backend renderer discriminator for operation results', () => {
		expect(
			getToolResultRenderer('custom', 'files.search', { results: [] }, 'files.search'),
		).toBeTruthy();
	});

	it('does not treat an array as a files result record', () => {
		expect(getToolResultRenderer('custom', 'files', [{ media: {} }], 'files')).toBe(
			ToolJsonResult,
		);
	});

	it('falls back to JsonView for malformed builtin nested records and array items', () => {
		const malformedResults: Array<{
			renderer: string;
			data: Record<string, unknown>;
		}> = [
			{ renderer: 'files.search', data: { results: [null] } },
			{ renderer: 'files.search', data: { results: null } },
			{ renderer: 'files.search', data: { results: [{ path: 'a.rs', snippet: 42 }] } },
			{ renderer: 'files.search', data: { results: [], mode: 'future-mode' } },
			{ renderer: 'files', data: { results: [{ path: 42 }] } },
			{ renderer: 'files', data: { operation: 'future-operation' } },
			{ renderer: 'files', data: { symbols: [{ kind: 'macro' }] } },
			{ renderer: 'files', data: { media: [] } },
			{ renderer: 'media', data: { media: { available_representations: [null] } } },
			{ renderer: 'media', data: { representation: 'unknown_representation' } },
			{ renderer: 'media', data: { operation: 'future-operation' } },
			{ renderer: 'media', data: { modality: 'archive' } },
			{ renderer: 'media', data: { file_kind: 'package' } },
			{
				renderer: 'media',
				data: { media: { available_representations: ['managed_file_ref', 'future'] } },
			},
			{ renderer: 'agent', data: { agents: [null] } },
			{ renderer: 'agent', data: { agents: [{ name: 'peer', status: 'future' }] } },
			{ renderer: 'process', data: { processes: [null] } },
			{ renderer: 'process', data: { processes: null } },
			{ renderer: 'process', data: { operation: 'restart' } },
			{ renderer: 'process', data: { processes: [{ status: 'Unknown(73)' }] } },
			{ renderer: 'clipboard', data: { entries: [null] } },
			{ renderer: 'clipboard', data: { entries: null } },
			{ renderer: 'input', data: { operation: 'click', clicked: [12, '20'] } },
			{ renderer: 'input', data: { operation: 'restart' } },
			{ renderer: 'input', data: { operation: 'click', button: 'primary' } },
			{ renderer: 'window', data: { elements: [null] } },
			{ renderer: 'window', data: { operation: 'restart' } },
			{ renderer: 'window', data: { operation: 'wait', condition: 'visible' } },
			{ renderer: 'window', data: { operation: 'screenshot', format: 'jpeg' } },
			{ renderer: 'window', data: { elements: [{ control_type: 'FutureControl' }] } },
			{ renderer: 'tool_runs', data: { tool_runs: [null] } },
			{ renderer: 'tool_runs', data: { operation: 'pause' } },
			{ renderer: 'tool_runs', data: { tool_run_id: 'toolrun-1', status: 'future-status' } },
			{
				renderer: 'tool_runs',
				data: { tool_runs: [{ tool_run_id: 'toolrun-1', status: 'future-status' }] },
			},
			{ renderer: 'schedule', data: { scheduled_tool_runs: [null] } },
			{ renderer: 'schedule', data: { operation: 'pause' } },
			{
				renderer: 'schedule',
				data: { scheduled_tool_runs: [{ tool_run_id: 'toolrun-1', due_at: 42 }] },
			},
			{
				renderer: 'schedule',
				data: {
					scheduled_tool_runs: [
						{
							tool_run_id: 'toolrun-1',
							title: 'test',
							body: 'test',
							mode: 'future',
							due_at: '',
						},
					],
				},
			},
			{ renderer: 'schedule', data: { operation: 'set', mode: 'future' } },
			{ renderer: 'system', data: { os: [] } },
			{ renderer: 'system', data: { networks: [{ ips: null }] } },
			{ renderer: 'system', data: { scope: 'future-scope' } },
			{ renderer: 'system', data: { scope: { unexpected: true } } },
			{ renderer: 'system', data: { scope: 'process', processes: [null] } },
			{ renderer: 'system', data: { scope: 'power', ac_power: 'plugged' } },
			{ renderer: 'system', data: { scope: 'power', battery_status: 'empty' } },
			{ renderer: 'haven_mcp', data: { servers: [null] } },
			{
				renderer: 'haven_diagnostics',
				data: { sessions: [{ id: 'ses-1', status: 'future' }] },
			},
			{ renderer: 'http', data: { status: '200' } },
			{ renderer: 'http', data: {} },
			{ renderer: 'http', data: { status: null } },
			{ renderer: 'http', data: { status: 200, body: { unexpected: true } } },
			{ renderer: 'web_search', data: { results: [null] } },
			{ renderer: 'web_search', data: { queries: null } },
			{ renderer: 'web_search', data: { results: [{ title: 'x', url: 'https://a.test', snippet: 42 }] } },
			{ renderer: 'memory', data: { facts: [null] } },
			{ renderer: 'memory', data: { facts: [{ tags: 42 }] } },
			{ renderer: 'memory', data: { operation: 'future-operation' } },
			{ renderer: 'memory', data: { facts: [{ confidence: '0.75' }] } },
			{ renderer: 'memory', data: { hits: null } },
			{ renderer: 'memory', data: { hits: [{ score: '0.9' }] } },
			{ renderer: 'memory', data: { hits: [], mode: 'future-mode' } },
		];

		for (const { renderer, data } of malformedResults) {
			expect(getToolResultRenderer('custom', renderer, data, renderer)).toBe(ToolJsonResult);
		}
	});

	it('ignores malformed system operation metadata unused by the system renderer', () => {
		expect(
			getToolResultRenderer(
				'custom',
				'system',
				{ scope: 'info', operation: { unexpected: true }, os: { name: 'Windows' } },
				'system',
			),
		).toBe(ToolSystemResult);
	});

	it('keeps valid builtin shapes specialized and unknown extension renderers open', () => {
		expect(
			getToolResultRenderer(
				'custom',
				'http',
				{ status: 200, truncated: null, body: null },
				'http',
			),
		).toBe(ToolHttpResult);
		expect(
			getToolResultRenderer(
				'custom',
				'memory',
				{
					mode: null,
					facts: [
						{
							id: null,
							subject: null,
							predicate: null,
							object: null,
							confidence: null,
							tags: null,
							source_snippet: null,
						},
					],
					stored: null,
					deleted: null,
				},
				'memory',
			),
		).toBe(ToolMemoryResult);
		expect(
			getToolResultRenderer(
				'custom',
				'files.search',
				{
					count: null,
					mode: null,
					results: [{ path: 'a.rs', line: null, snippet: null }],
				},
				'files.search',
			),
		).toBe(ToolFileSearchResult);
		expect(
			getToolResultRenderer(
				'custom',
				'web_search',
				{ label: null, results: [{ title: 'A', url: 'https://a.test', snippet: null }] },
				'web_search',
			),
		).toBe(ToolWebSearchResult);
		expect(
			getToolResultRenderer(
				'custom',
				'files',
				{ operation: 'outline', symbols: [{ kind: 'heading', name: 'Overview' }] },
				'files',
			),
		).toBe(ToolFileResult);
		expect(
			getToolResultRenderer(
				'custom',
				'input',
				{ operation: 'click_element', button: 'right' },
				'input',
			),
		).toBe(ToolInputResult);
		expect(
			getToolResultRenderer(
				'custom',
				'files',
				{ operation: 'read', media: { asset_id: 'asset-1', content: 'plain text' } },
				'files',
			),
		).toBe(ToolMediaResult);
		expect(
			getToolResultRenderer(
				'custom',
				'window',
				{
					operation: 'wait',
					condition: 'title_contains',
					elements: [{ control_type: 'Unknown' }],
				},
				'window',
			),
		).toBe(ToolWindowResult);
		expect(
			getToolResultRenderer(
				'custom',
				'tool_runs',
				{ operation: 'tool_runs_result_injected', status: 'completed' },
				'tool_runs',
			),
		).toBe(ToolRunsResult);
		expect(
			getToolResultRenderer(
				'custom',
				'schedule',
				{ operation: 'schedule_set', tool_run_id: 'toolrun-1', mode: 'tool' },
				'schedule',
			),
		).toBe(ToolScheduleResult);
		expect(
			getToolResultRenderer(
				'custom',
				'clipboard',
				{ entries: [{ content: 'copied text', timestamp_ms: 'not rendered' }] },
				'clipboard',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'media',
				{
					representation: 'document_pages',
					modality: 'document',
					file_kind: 'document',
					media: {
						representation: 'managed_file_ref',
						available_representations: ['managed_file_ref', 'document_pages'],
						content: { opaque: true },
					},
				},
				'media',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'schedule',
				{
					scheduled_tool_runs: [
						{
							tool_run_id: 'toolrun-1',
							title: 'scheduled',
							body: 'body',
							mode: 'continue',
							due_at: '',
							tool_args: [null],
						},
					],
					operation: 'set',
					mode: 'tool',
					tool_run_id: 'toolrun-2',
					fires_at: '',
					title: [],
				},
				'schedule',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'messaging',
				{
					agents: [
						{
							name: 'peer',
							title: null,
							role: null,
							status: 'online',
							last_seen: { opaque: true },
							capabilities: 'not rendered',
						},
					],
				},
				'agent',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'haven_diagnostics',
				{ sessions: [{ id: 'ses-1', status: 'running' }] },
				'haven_diagnostics',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'haven_mcp',
				{
					servers: [
						{
							name: 'server',
							connected: true,
							tools: 2,
							status: { unexpected: true },
						},
					],
				},
				'haven_mcp',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'process',
				{
					processes: [{ pid: 42, name: 'haven', cpu: 1, memory: 2, status: 'Run' }],
					matching_count: 'not rendered',
					name_filter: [],
				},
				'process',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer(
				'custom',
				'system',
				{
					scope: 'process',
					processes: [{ pid: 42, name: 'haven', cpu: 1, memory: 2, status: 'Run' }],
				},
				'system',
			),
		).not.toBe(ToolJsonResult);
		expect(
			getToolResultRenderer('custom', 'mcp__filesystem__read', { extra: [null] }, 'mcp'),
		).toBe(ToolJsonResult);
	});

	it('keeps shell streaming placeholders and falls back for malformed shell metadata', () => {
		expect(getToolResultRenderer('shell', 'shell', null)).toBe(ToolShellResult);
		expect(getToolResultRenderer('shell', 'shell', { truncated: 'yes' })).toBe(ToolJsonResult);
		expect(
			getToolResultRenderer('shell', 'shell', {
				execution_mode: 'background',
				status: 'running',
				tool_run_id: 'toolrun-1',
			}),
		).toBe(ToolShellResult);
		expect(
			getToolResultRenderer('shell', 'shell', {
				execution_mode: 'background',
				status: 'future',
			}),
		).toBe(ToolJsonResult);
		expect(getToolResultRenderer('shell', 'shell', { execution_mode: 'detached' })).toBe(
			ToolJsonResult,
		);
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
		expect(parseToolResult('web_search', payload, 'web_search')).toEqual({
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
	it('uses the web_search renderer for query-only results', () => {
		const payload = JSON.stringify({ label: '已联网搜索', queries: ['foo'], results: [] });
		expect(parseToolResult('web_search', payload, 'web_search')?.kind).toBe('custom');
	});
	it('uses the generic renderer when a result has no renderer contract', () => {
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

	it('lets a pending ask be hidden without resolving or ignoring it', async () => {
		const onAskDismiss = vi.fn();
		const onIgnore = vi.fn();
		const onAskSubmit = vi.fn();
		const { container } = render(ToolResultCard, {
			type: 'ask',
			content: '选择？',
			options: ['方案 A'],
			awaiting: true,
			messageId: 'ask-dismiss',
			onAskDismiss,
			onIgnore,
			onAskSubmit,
		});

		expect(container.querySelector('[data-interaction-id="ask-dismiss"]')).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '收起问题' }));
		expect(onAskDismiss).toHaveBeenCalledWith('ask-dismiss');
		expect(onIgnore).not.toHaveBeenCalled();
		expect(onAskSubmit).not.toHaveBeenCalled();
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
	it.each([
		['completed', '执行成功'],
		['succeeded', '执行成功'],
		['failed', '调用失败'],
	])('shows %s in the shared header status position', (outcome, label) => {
		const { container } = render(ToolResultCard, {
			type: 'tool',
			toolName: 'messaging',
			content: 'tool output',
			outcome,
		});

		const headerState = container.querySelector('.tool-header-state');
		expect(headerState).toBeTruthy();
		expect(headerState?.textContent).toContain(label);
		expect(headerState?.getAttribute('data-state')).toBe(
			outcome === 'succeeded' ? 'completed' : outcome,
		);
		expect(container.querySelector('[data-detail="status"]')).toBeNull();
	});

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

	it('distinguishes an empty successful result from an empty failed call', async () => {
		const successful = render(ToolResultCard, {
			toolName: 'files',
			content: '',
			outcome: 'succeeded',
		});
		await expandToolCard(successful.container);
		expect(
			successful.container.querySelector('[data-detail="output"] .tool-result-message')
				?.textContent,
		).toBe('（无结果）');

		successful.unmount();
		const failed = render(ToolResultCard, {
			toolName: 'files',
			content: '',
			outcome: 'failed',
		});
		await expandToolCard(failed.container);
		expect(
			failed.container.querySelector('[data-detail="output"] .tool-result-message')
				?.textContent,
		).toBe('调用失败');
		expect(failed.container.querySelector('.tool-result-message--error')).toBeTruthy();

		failed.unmount();
		const timedOut = render(ToolResultCard, {
			toolName: 'files',
			content: '',
			outcome: 'timed_out_and_terminated',
		});
		await expandToolCard(timedOut.container);
		expect(
			timedOut.container.querySelector('[data-detail="output"] .tool-result-message')
				?.textContent,
		).toBe('调用超时');
	});

	it('uses the result envelope when the outcome prop is absent', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: '',
			result: { outcome: 'failed' },
		});
		await expandToolCard(container);
		expect(
			container.querySelector('[data-detail="output"] .tool-result-message')?.textContent,
		).toBe('调用失败');
	});

	it.each([
		['timed_out_and_terminated', '执行超时', 'timed_out'],
		['timed_out_unknown', '结果未知，可能已执行', 'unknown'],
	])('projects canonical result outcome %s into the card status', (outcome, label, state) => {
		const { container } = render(ToolResultCard, {
			toolName: 'messaging',
			content: 'request timed out',
			result: { outcome },
		});

		const headerState = container.querySelector('.tool-header-state');
		expect(headerState?.textContent).toContain(label);
		expect(headerState?.getAttribute('data-state')).toBe(state);
	});
});

describe('ToolResultCard shell / notify / generic', () => {
	it('renders plain shell output in a terminal card', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: 'Hello from cmd',
		});
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
		expect(container.querySelector('.tool-result-preview')!.textContent).toBe('line1\nline2');
		expect(screen.getByText('输出过长已截断')).toBeTruthy();
	});

	it('renders the exit code for a completed background shell ToolRun', async () => {
		upsertToolRun({
			toolRunId: 'toolrun-shell-1',
			kind: 'background',
			status: 'completed',
			output: 'command output',
			exitCode: 0,
		});
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			toolRunId: 'toolrun-shell-1',
			content: '',
		});
		await expandToolCard(container);
		expect(container.querySelector('.tool-header-state')?.textContent).toContain('执行成功');
		expect(container.querySelector('.tool-result-label')?.textContent).toContain(
			'后台任务已完成',
		);
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
	toolRunStore.set({});
	clearToolOutputPreviewsForSession(null);
});

describe('ToolResultCard raw', () => {
	it('renders plain text output in a raw card with the tool label', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'media',
			content: 'some plain text',
		});
		await expandToolCard(container);
		expect(screen.getByText('媒体')).toBeTruthy();
		expect(container.querySelector('.tool-result-preview')!.textContent).toContain(
			'some plain text',
		);
	});

	it('pretty-prints JSON array observations', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'unknown_tool',
			content: JSON.stringify([1, 2, { a: 'b' }]),
		});
		await expandToolCard(container);
		expect(container.querySelector('.tool-result-preview')!.textContent).toContain('"a"');
		expect(container.querySelector('.tool-result-preview')!.textContent).toContain('"b"');
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

	it('labels Skill tools and accepts resume tool_input JSON strings', async () => {
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
		setToolOutputPreview(messageId, 'first chunk');
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
		clearToolOutputPreview(messageId);
		await tick();
		setToolOutputPreview(messageId, 'second chunk');
		await tick();

		expect(header.getAttribute('aria-expanded')).toBe('false');
	});
});

describe('ToolResultCard files', () => {
	it('renders tool hints only when the dynamic value is text', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				operation: 'create_dir',
				created: true,
				path: 'D:\\tmp\\reports',
				hint: { unexpected: 'object' },
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('已创建目录')).toBeTruthy();
		expect(container.querySelector('.tool-card-hint')).toBeNull();
	});

	it('renders create_dir with the file-specific result UI', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				operation: 'create_dir',
				created: true,
				path: 'D:\\tmp\\reports',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('已创建目录')).toBeTruthy();
		expect(screen.getByText('D:\\tmp\\reports')).toBeTruthy();
	});

	it('does not route a removed file_search alias to the files renderer', () => {
		expect(
			parseToolResult('file_search', searchJson([{ path: 'D:\\tmp\\match.rs' }])),
		).toMatchObject({
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

		expect(container.querySelectorAll('.tool-result-search-row')).toHaveLength(15);
		const moreButton = screen.getByRole('button', { name: '显示更多（剩余 235 条）' });
		expect(moreButton).toBeTruthy();

		await fireEvent.click(moreButton);
		expect(container.querySelectorAll('.tool-result-search-row')).toHaveLength(30);
		expect(screen.getByRole('button', { name: '显示更多（剩余 220 条）' })).toBeTruthy();
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

	it('distinguishes an unsampled overview CPU from zero usage', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ cpu: { cores: 8, logical_cpus: 16 } }),
		});
		await expandToolCard(container);
		expect(screen.getByText('本次概览未采样')).toBeTruthy();
		expect(screen.queryByText('0.0%')).toBeNull();
		expect(screen.getByText('8 核 / 16 线程')).toBeTruthy();
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
	it('routes ToolRun operations to their dedicated result renderer', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'tool_runs.cancel',
			renderer: 'tool_runs',
			content: JSON.stringify({
				operation: 'tool_runs_cancel',
				tool_run_id: 'toolrun-2',
				cancelled: true,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('toolrun-2')).toBeTruthy();
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
					{ pid: 4, name: 'd.exe', status: 'Unknown' },
				],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('运行中')).toBeTruthy();
		expect(screen.getByText('休眠')).toBeTruthy();
		expect(screen.getByText('僵尸')).toBeTruthy();
		expect(screen.getByText('未知')).toBeTruthy();
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

	it('paginates processes in the shared fifteen-row pages', async () => {
		const processes = Array.from({ length: 60 }, (_, i) => ({ pid: i + 1, name: `p${i}.exe` }));
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({ scope: 'process', processes }),
		});
		await expandToolCard(container);
		expect(screen.getByText('60 个进程')).toBeTruthy();
		expect(screen.getByText('p14.exe')).toBeTruthy();
		expect(screen.queryByText('p15.exe')).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: '显示更多（剩余 45 条）' }));
		expect(screen.getByText('p29.exe')).toBeTruthy();
		expect(screen.queryByText('p30.exe')).toBeNull();
	});
});

describe('ToolResultCard tool_runs', () => {
	it('renders the ToolRun id with a completed badge', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'tool_runs.list',
			renderer: 'tool_runs',
			content: JSON.stringify({
				operation: 'tool_runs_list',
				tool_run_id: 'toolrun-1',
				status: 'completed',
				exit_code: 0,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('toolrun-1')).toBeTruthy();
		expect(screen.getByText('已完成')).toBeTruthy();
		expect(screen.getByText('退出码 0')).toBeTruthy();
	});

	it('renders not-found inspect status as a query result', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'tool_runs.inspect',
			renderer: 'tool_runs',
			content: JSON.stringify({
				operation: 'inspect',
				tool_run_id: 'toolrun-missing',
				status: 'not_found',
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('未找到')).toBeTruthy();
	});

	it('renders cancel results with an explicit status', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'tool_runs.cancel',
			renderer: 'tool_runs',
			content: JSON.stringify({
				operation: 'tool_runs_cancel',
				tool_run_id: 'toolrun-2',
				cancelled: true,
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('toolrun-2')).toBeTruthy();
	});

	it('renders list rows with generated ToolRun statuses', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'tool_runs.list',
			renderer: 'tool_runs',
			content: JSON.stringify({
				operation: 'tool_runs_list',
				tool_runs: [{ tool_run_id: 'toolrun-3', status: 'running' }],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('toolrun-3')).toBeTruthy();
		expect(screen.getByText('运行中')).toBeTruthy();
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

	it('renders window OCR through the operation renderer', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'window.ocr',
			renderer: 'window',
			content: JSON.stringify({
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
		expect(screen.getByText('OCR 完成')).toBeTruthy();
		expect(screen.getByText('asset-0123456789abcdef0123456789abcdef')).toBeTruthy();
		expect(screen.getByText('窗口中的文字')).toBeTruthy();
		expect(container.querySelector('.tool-result-preview')?.textContent).toBe('窗口中的文字');
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
	it('renders a single scheduled tool result with tool_run_id, mode and fires_at', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'schedule.set',
			renderer: 'schedule',
			content: JSON.stringify({
				operation: 'schedule_set',
				tool_run_id: 'r42',
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
			toolName: 'schedule.cancel',
			renderer: 'schedule',
			content: JSON.stringify({ operation: 'schedule_cancel', tool_run_id: 'toolrun-42' }),
		});
		await expandToolCard(container);
		expect(screen.getByText('已取消')).toBeTruthy();
		expect(screen.getByText('#toolrun-42')).toBeTruthy();
	});

	it('renders list due_at from the scheduled ToolRun projection', async () => {
		const { container } = render(ToolResultCard, {
			toolName: 'schedule.list',
			renderer: 'schedule',
			content: JSON.stringify({
				operation: 'schedule_list',
				scheduled_tool_runs: [
					{
						tool_run_id: 'toolrun-scheduled',
						title: '每日摘要',
						body: '生成今日摘要',
						mode: 'continue',
						due_at: '2026-10-09T09:00:00+08:00',
					},
				],
			}),
		});
		await expandToolCard(container);
		expect(screen.getByText('每日摘要')).toBeTruthy();
		expect(screen.getByText('继续会话')).toBeTruthy();
		expect(screen.getByText('2026-10-09T09:00:00+08:00')).toBeTruthy();
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
			toolName: 'haven.tools.tool_disable',
			renderer: 'haven_tools',
			content: JSON.stringify({
				operation: 'tool_disable',
				name: 'files',
				enabled: false,
				saved: true,
			}),
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
			content: JSON.stringify({
				scope: 'input',
				operation: 'click',
				clicked: [10, 20],
				button: 'left',
			}),
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
