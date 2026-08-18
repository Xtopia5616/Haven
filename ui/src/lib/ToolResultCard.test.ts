import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import ToolResultCard, { canRenderToolResult, parseToolResult } from './ToolResultCard.svelte';

const searchJson = (results: any[], extra: any = {}) =>
	JSON.stringify({ results, count: results.length, mode: 'filename', ...extra });

describe('canRenderToolResult', () => {
	it('accepts search with a results array', () => {
		expect(canRenderToolResult('files', searchJson([{ path: 'a.rs' }]))).toBe(true);
	});
	it('accepts system, process, window, status, reminder, env, file, network, clipboard, power', () => {
		expect(canRenderToolResult('system', JSON.stringify({ cpu: { usage_pct: 12 } }))).toBe(
			true,
		);
		expect(canRenderToolResult('process', JSON.stringify({ processes: [{ pid: 1 }] }))).toBe(
			true,
		);
		expect(canRenderToolResult('window', JSON.stringify({ windows: [{ title: 'x' }] }))).toBe(
			true,
		);
		expect(canRenderToolResult('action_status', JSON.stringify({ status: 'running' }))).toBe(true);
		expect(canRenderToolResult('schedule', JSON.stringify({ reminders: [] }))).toBe(true);
		expect(canRenderToolResult('schedule', JSON.stringify({ id: 'r1', mode: 'notify' }))).toBe(
			true,
		);
		expect(canRenderToolResult('env', JSON.stringify({ variables: [] }))).toBe(true);
		expect(canRenderToolResult('file', JSON.stringify({ written: true, path: 'x' }))).toBe(
			true,
		);
		expect(canRenderToolResult('network', JSON.stringify({ status: 200 }))).toBe(true);
		expect(canRenderToolResult('clipboard', JSON.stringify({ content: 'hi' }))).toBe(true);
		expect(canRenderToolResult('power', JSON.stringify({ battery_percent: 80 }))).toBe(true);
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
		expect(canRenderToolResult('audio', JSON.stringify({ played: true }))).toBe(true);
		expect(canRenderToolResult('files', JSON.stringify({ nope: 1 }))).toBe(true);
	});
	it('accepts any non-empty text as a raw card', () => {
		expect(canRenderToolResult('files', '{not json[... truncated')).toBe(true);
		expect(canRenderToolResult('audio', 'plain text')).toBe(true);
		expect(canRenderToolResult('notify', 'Some other text')).toBe(true);
	});
	it('rejects empty content', () => {
		expect(canRenderToolResult('', '')).toBe(false);
		expect(canRenderToolResult('files', '')).toBe(false);
	});
});

describe('parseToolResult', () => {
	it('returns null for empty content', () => {
		expect(parseToolResult('files', '')).toBeNull();
	});
	it('classifies non-JSON text, arrays and primitives as raw', () => {
		expect(parseToolResult('files', 'plain text')).toEqual({ kind: 'raw', data: null });
		expect(parseToolResult('action_status', JSON.stringify([1, 2]))).toEqual({
			kind: 'raw',
			data: [1, 2],
		});
		expect(parseToolResult('action_status', '42')).toEqual({ kind: 'raw', data: 42 });
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
		expect(screen.getByText('Haven 需要你确认')).toBeTruthy();
		expect(screen.getByText('你想怎么做？')).toBeTruthy();
		expect(screen.getByText('等待你的回答...')).toBeTruthy();
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

	it('fires onQuickReply with the message id and clicked option', async () => {
		const onQuickReply = vi.fn();
		render(ToolResultCard, {
			type: 'ask',
			content: '选择？',
			options: ['立即执行'],
			awaiting: true,
			messageId: 'ask-42',
			onQuickReply,
		});
		await fireEvent.click(screen.getByText('立即执行'));
		expect(onQuickReply).toHaveBeenCalledWith('ask-42', '立即执行');
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

describe('ToolResultCard shell / notify / generic', () => {
	it('renders plain shell output in a terminal card', () => {
		render(ToolResultCard, { toolName: 'shell', content: 'Hello from cmd' });
		expect(screen.getByText('终端输出')).toBeTruthy();
		expect(screen.getByText('Hello from cmd')).toBeTruthy();
	});

	it('renders JSON shell output with the truncated note', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'shell',
			content: JSON.stringify({ output: 'line1\nline2', truncated: true }),
		});
		expect(container.querySelector('.content-preview')!.textContent).toBe('line1\nline2');
		expect(screen.getByText('输出过长已截断')).toBeTruthy();
	});

	it('renders a notification card with title and body', () => {
		render(ToolResultCard, {
			toolName: 'notify',
			content: 'Notification sent: 构建完成: 全部测试通过',
		});
		expect(screen.getByText('通知')).toBeTruthy();
		expect(screen.getByText('构建完成')).toBeTruthy();
		expect(screen.getByText('全部测试通过')).toBeTruthy();
	});

	it('renders a generic JSON tree card for tools without a custom shape', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'load_mcp',
			content: JSON.stringify({ server_name: 'filesystem', status: 'loaded' }),
		});
		expect(screen.getByText('加载 MCP')).toBeTruthy();
		expect(container.querySelector('.jv-view')).toBeTruthy();
		expect(screen.getByText('"server_name"')).toBeTruthy();
		expect(screen.getByText('"filesystem"')).toBeTruthy();
	});
});

describe('ToolResultCard raw', () => {
	it('renders plain text output in a raw card with the tool label', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'audio',
			content: 'some plain text',
		});
		expect(screen.getByText('音频')).toBeTruthy();
		expect(container.querySelector('.content-preview')!.textContent).toContain(
			'some plain text',
		);
	});

	it('pretty-prints JSON array observations', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'action_status',
			content: JSON.stringify([1, 2, { a: 'b' }]),
		});
		expect(container.querySelector('.content-preview')!.textContent).toContain('"a"');
		expect(container.querySelector('.content-preview')!.textContent).toContain('"b"');
	});
});

describe('ToolResultCard collapsible', () => {
	it('renders collapsed once the observation is final', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
		});
		const details = container.querySelector(
			'details.tool-card',
		) as HTMLDetailsElement;
		expect(details).toBeTruthy();
		expect(details.open).toBe(false);
	});

	it('expands while streaming and auto-collapses when streaming ends', async () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
			streaming: true,
		});
		const details = container.querySelector(
			'details.tool-card',
		) as HTMLDetailsElement;
		expect(details.open).toBe(true);
		await rerender({ streaming: false });
		expect(details.open).toBe(false);
	});

	it('toggles open when the header is clicked and keeps a manual expand', async () => {
		const { container, rerender } = render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a.rs' }]),
		});
		const details = container.querySelector(
			'details.tool-card',
		) as HTMLDetailsElement;
		expect(details.open).toBe(false);
		await fireEvent.click(details.querySelector('summary')!);
		expect(details.open).toBe(true);
		await rerender({ content: searchJson([{ path: 'b.rs' }]) });
		expect(details.open).toBe(true);
	});
});

describe('ToolResultCard files', () => {
	it('renders a filename-mode search card with paths and count', () => {
		render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'D:\\workspace\\a.rs' }, { path: 'D:\\workspace\\b.rs' }]),
		});
		expect(screen.getByText('文件与搜索')).toBeTruthy();
		expect(screen.getByText('2 个结果 · 文件名')).toBeTruthy();
		expect(screen.getByText('D:\\workspace\\a.rs')).toBeTruthy();
		expect(screen.getByText('D:\\workspace\\b.rs')).toBeTruthy();
	});

	it('renders line numbers and snippets in content mode', () => {
		render(ToolResultCard, {
			toolName: 'files',
			content: JSON.stringify({
				results: [{ path: 'lib.rs', line: 42, snippet: 'fn main() {}' }],
				count: 1,
				mode: 'content',
			}),
		});
		expect(screen.getByText('1 个结果 · 全文')).toBeTruthy();
		expect(screen.getByText('L42')).toBeTruthy();
		expect(screen.getByText('fn main() {}')).toBeTruthy();
	});

	it('renders the truncated hint when present', () => {
		render(ToolResultCard, {
			toolName: 'files',
			content: searchJson([{ path: 'a' }], { hint: 'Results hit the max_results cap.' }),
		});
		expect(screen.getByText('Results hit the max_results cap.')).toBeTruthy();
	});
});

describe('ToolResultCard system', () => {
	it('renders cpu/memory meters and os info', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'system',
			content: JSON.stringify({
				os: { name: 'Windows 11', hostname: 'DESKTOP-X', uptime_secs: 90000 },
				cpu: { brand: 'Ryzen', cores: 8, logical_cpus: 16, usage_pct: 25.5 },
				memory: { total_bytes: 16 * 1024 ** 3, used_bytes: 8 * 1024 ** 3 },
			}),
		});
		expect(screen.getByText('系统信息')).toBeTruthy();
		expect(screen.getByText('Windows 11')).toBeTruthy();
		expect(screen.getByText('DESKTOP-X')).toBeTruthy();
		expect(screen.getByText('25.5%')).toBeTruthy();
		expect(screen.getByText('8.0 GB / 16.0 GB')).toBeTruthy();
		expect(screen.getByText('8 核 / 16 线程')).toBeTruthy();
		expect(container.querySelectorAll('.meter-fill').length).toBe(2);
	});
});

describe('ToolResultCard process', () => {
	it('renders a process table with pid, cpu and memory', () => {
		render(ToolResultCard, {
			toolName: 'process',
			content: JSON.stringify({
				processes: [
					{ pid: 100, name: 'chrome.exe', cpu: 3.5, memory: 500 * 1024 * 1024 },
					{ pid: 200, name: 'explorer.exe', cpu: 0.2, memory: 200 * 1024 * 1024 },
				],
			}),
		});
		expect(screen.getByText('2 个进程')).toBeTruthy();
		expect(screen.getByText('chrome.exe')).toBeTruthy();
		expect(screen.getByText('explorer.exe')).toBeTruthy();
		expect(screen.getByText('500 MB')).toBeTruthy();
	});

	it('renders a status badge column with mapped labels', () => {
		render(ToolResultCard, {
			toolName: 'process',
			content: JSON.stringify({
				processes: [
					{ pid: 1, name: 'a.exe', status: 'Run' },
					{ pid: 2, name: 'b.exe', status: 'Sleep' },
					{ pid: 3, name: 'c.exe', status: 'Zombie' },
				],
			}),
		});
		expect(screen.getByText('运行中')).toBeTruthy();
		expect(screen.getByText('休眠')).toBeTruthy();
		expect(screen.getByText('僵尸')).toBeTruthy();
	});

	it('filters processes by name', async () => {
		render(ToolResultCard, {
			toolName: 'process',
			content: JSON.stringify({
				processes: [
					{ pid: 1, name: 'chrome.exe' },
					{ pid: 2, name: 'explorer.exe' },
				],
			}),
		});
		await fireEvent.input(screen.getByPlaceholderText('筛选进程...'), {
			target: { value: 'chrome' },
		});
		expect(screen.getByText('1 / 2 个进程')).toBeTruthy();
		expect(screen.getByText('chrome.exe')).toBeTruthy();
		expect(screen.queryByText('explorer.exe')).toBeNull();
	});

	it('collapses beyond 50 processes with a show-all toggle', async () => {
		const processes = Array.from({ length: 60 }, (_, i) => ({ pid: i + 1, name: `p${i}.exe` }));
		render(ToolResultCard, {
			toolName: 'process',
			content: JSON.stringify({ processes }),
		});
		expect(screen.getByText('60 个进程')).toBeTruthy();
		expect(screen.queryByText('p59.exe')).toBeNull();
		await fireEvent.click(screen.getByText('显示全部 60 个进程'));
		expect(screen.getByText('p59.exe')).toBeTruthy();
		await fireEvent.click(screen.getByText('收起'));
		expect(screen.queryByText('p59.exe')).toBeNull();
	});
});

describe('ToolResultCard status', () => {
	it('renders the job id with a completed badge', () => {
		render(ToolResultCard, {
			toolName: 'action_status',
			content: JSON.stringify({ job_id: 'job-1', status: 'completed', exit_code: 0 }),
		});
		expect(screen.getByText('job-1')).toBeTruthy();
		expect(screen.getByText('completed')).toBeTruthy();
		expect(screen.getByText('退出码 0')).toBeTruthy();
	});
});

describe('ToolResultCard file', () => {
	it('renders write / delete results', () => {
		render(ToolResultCard, {
			toolName: 'file',
			content: JSON.stringify({ written: true, path: 'C:\\tmp\\out.txt' }),
		});
		expect(screen.getByText('已写入')).toBeTruthy();
		expect(screen.getByText('C:\\tmp\\out.txt')).toBeTruthy();
	});

	it('renders directory listing entries', () => {
		render(ToolResultCard, {
			toolName: 'file',
			content: JSON.stringify({ entries: ['a.txt', 'b.rs'], count: 2 }),
		});
		expect(screen.getByText('2 项')).toBeTruthy();
		expect(screen.getByText('a.txt')).toBeTruthy();
		expect(screen.getByText('b.rs')).toBeTruthy();
	});
});

describe('ToolResultCard network', () => {
	it('renders status badge and body preview', () => {
		render(ToolResultCard, {
			toolName: 'network',
			content: JSON.stringify({ status: 200, body: '{"ok":true}', truncated: false }),
		});
		expect(screen.getByText('200')).toBeTruthy();
		expect(screen.getByText('{"ok":true}')).toBeTruthy();
	});

	it('marks non-2xx status as failed', () => {
		const { container } = render(ToolResultCard, {
			toolName: 'network',
			content: JSON.stringify({ status: 404, body: '' }),
		});
		expect(container.querySelector('.status-failed')).toBeTruthy();
	});
	it('renders a single reminder set result with id, mode and fires_at', () => {
		render(ToolResultCard, {
			toolName: 'schedule',
			content: JSON.stringify({
				id: 'r42',
				mode: 'tool',
				fires_at: '2026-08-05T09:00:00+08:00',
				wakes_session: true,
			}),
		});
		expect(screen.getByText('#r42')).toBeTruthy();
		expect(screen.getByText('tool')).toBeTruthy();
		expect(screen.getByText('触发时间 2026-08-05T09:00:00+08:00')).toBeTruthy();
	});
});

describe('ToolResultCard env', () => {
	it('renders a variables list', () => {
		render(ToolResultCard, {
			toolName: 'env',
			content: JSON.stringify({ variables: [{ name: 'PATH', value: 'C:\\bin' }], count: 1 }),
		});
		expect(screen.getByText('1 个变量')).toBeTruthy();
		expect(screen.getByText('PATH')).toBeTruthy();
		expect(screen.getByText('C:\\bin')).toBeTruthy();
	});

	it('filters variables by name and value', async () => {
		render(ToolResultCard, {
			toolName: 'env',
			content: JSON.stringify({
				variables: [
					{ name: 'PATH', value: 'C:\\bin' },
					{ name: 'HOME', value: 'C:\\Users\\me' },
				],
			}),
		});
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
		render(ToolResultCard, {
			toolName: 'env',
			content: JSON.stringify({ variables: [{ name: 'API_KEY', value: 'secret-value' }] }),
		});
		await fireEvent.click(screen.getByTitle('复制值'));
		expect(writeText).toHaveBeenCalledWith('secret-value');
	});
});
