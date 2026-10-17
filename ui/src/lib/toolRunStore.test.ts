import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import { appSessionReducer } from './sessionReducer.ts';
import {
	toolRunStore,
	sessionToolRunStore,
	upsertToolRun,
	upsertSessionToolRun,
	removeToolRun,
	refreshToolRuns,
	refreshSessionToolRuns,
	setActiveSessionToolRun,
	finalizeBackgroundToolRunMessages,
	resetToolRunStoreForTest,
} from './toolRunStore.ts';

describe('upsertToolRun', () => {
	beforeEach(() => {
		resetToolRunStoreForTest();
	});

	it('keeps the explicit background create payload', () => {
		upsertToolRun({
			toolRunId: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
			kind: 'background',
			status: 'running',
			startedAt: '2026-01-01T00:00:00Z',
		});
		const row = get(toolRunStore)['toolrun-d409f06a31c9daee6d1a4f437a1f1f85'];
		expect(row.status).toBe('running');
		expect(row.kind).toBe('background');
		expect(row.toolRunId).toBe('toolrun-d409f06a31c9daee6d1a4f437a1f1f85');
	});

	it('shares one canonical payload between the live and session indexes', () => {
		const payload = {
			toolRunId: 'toolrun-d409f06a31c9daee6d1a4f437a1f1f85',
			kind: 'background' as const,
			status: 'running' as const,
			sessionId: 'ses-0123456789abcdef0123456789abcdef',
		};
		upsertToolRun(payload);
		upsertSessionToolRun(payload);

		expect(get(toolRunStore)[payload.toolRunId]).toBe(
			get(sessionToolRunStore)[payload.sessionId]?.[payload.toolRunId],
		);
	});

	it('keeps terminal history after a row leaves the live board', () => {
		const payload = {
			toolRunId: 'toolrun-799dd769de1342a01fddd367f57dd080',
			kind: 'background' as const,
			status: 'completed' as const,
			sessionId: 'ses-0123456789abcdef0123456789abcdef',
		};
		upsertToolRun(payload);
		upsertSessionToolRun(payload);
		removeToolRun(payload.toolRunId);

		expect(get(toolRunStore)[payload.toolRunId]).toBeUndefined();
		expect(get(sessionToolRunStore)[payload.sessionId]?.[payload.toolRunId]).toBeDefined();
	});

	it('keeps a session binding update after an evicted row', () => {
		upsertToolRun({
			toolRunId: 'toolrun-799dd769de1342a01fddd367f57dd080',
			kind: 'background',
			status: 'completed',
		});
		removeToolRun('toolrun-799dd769de1342a01fddd367f57dd080');
		upsertToolRun({
			toolRunId: 'toolrun-799dd769de1342a01fddd367f57dd080',
			kind: 'background',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
		});
		const row = get(toolRunStore)['toolrun-799dd769de1342a01fddd367f57dd080'];
		expect(row.status).toBeUndefined();
		expect(row.sessionId).toBe('ses-0ecf371384ab79d597760ca41f2ba21c');
	});

	it('keeps an explicit terminal status from tool_run:finished', () => {
		upsertToolRun({
			toolRunId: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
			kind: 'background',
			status: 'running',
		});
		upsertToolRun({
			toolRunId: 'toolrun-9e21f42cef1a7b941794b4db9fb85cb1',
			kind: 'background',
			status: 'completed',
			output: 'done',
		});
		const row = get(toolRunStore)['toolrun-9e21f42cef1a7b941794b4db9fb85cb1'];
		expect(row.status).toBe('completed');
		expect(row.output).toBe('done');
	});

	it('evicts terminal rows before live running rows when over capacity', () => {
		for (let i = 0; i < 70; i++) {
			upsertToolRun({
				toolRunId: `toolrun-${(10_000 + i).toString(16).padStart(32, '0')}`,
				kind: 'background',
				status: 'completed',
			});
		}
		upsertToolRun({
			toolRunId: 'toolrun-31fea33fc8d7689de5d5dc4b38c1e4bc',
			kind: 'background',
			status: 'running',
		});
		const store = get(toolRunStore);
		expect(store['toolrun-31fea33fc8d7689de5d5dc4b38c1e4bc']?.status).toBe('running');
		expect(Object.keys(store).length).toBeLessThanOrEqual(64);
	});

	it('removeToolRun drops the row', () => {
		upsertToolRun({
			toolRunId: 'toolrun-f7391f4e56282cae467ea5a88dc7a560',
			kind: 'background',
			status: 'running',
		});
		removeToolRun('toolrun-f7391f4e56282cae467ea5a88dc7a560');
		expect(get(toolRunStore)['toolrun-f7391f4e56282cae467ea5a88dc7a560']).toBeUndefined();
	});

	it('ignores an older ToolRun refresh response', async () => {
		resetToolRunStoreForTest();
		let resolveOlder!: (value: unknown) => void;
		let resolveNewer!: (value: unknown) => void;
		const older = new Promise((resolve) => {
			resolveOlder = resolve;
		});
		const newer = new Promise((resolve) => {
			resolveNewer = resolve;
		});
		vi.mocked(invoke)
			.mockReset()
			.mockImplementationOnce(() => older as never)
			.mockImplementationOnce(() => newer as never);

		const olderRefresh = refreshToolRuns();
		const newerRefresh = refreshToolRuns();
		resolveNewer([
			{
				tool_run_id: 'toolrun-2e15073133060c9395627f3f343d1baa',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await newerRefresh;
		resolveOlder([
			{
				tool_run_id: 'toolrun-289594ac70693b3600ba5be6b241a692',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await olderRefresh;

		expect(get(toolRunStore)['toolrun-2e15073133060c9395627f3f343d1baa']?.status).toBe(
			'waiting',
		);
		expect(get(toolRunStore)['toolrun-289594ac70693b3600ba5be6b241a692']).toBeUndefined();
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('does not let a refresh overwrite a lifecycle event received in flight', async () => {
		let resolveRefresh!: (value: unknown) => void;
		const refreshResponse = new Promise((resolve) => {
			resolveRefresh = resolve;
		});
		vi.mocked(invoke)
			.mockReset()
			.mockReturnValueOnce(refreshResponse as never);

		const refresh = refreshToolRuns();
		upsertToolRun({
			toolRunId: 'toolrun-016864084f6608895ba0e6ecb4ea39f8',
			kind: 'scheduled',
			status: 'running',
		});
		resolveRefresh([
			{
				tool_run_id: 'toolrun-016864084f6608895ba0e6ecb4ea39f8',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await refresh;

		expect(get(toolRunStore)['toolrun-016864084f6608895ba0e6ecb4ea39f8']?.status).toBe(
			'running',
		);
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('fences a live-board refresh when a session lifecycle event updates the shared payload', async () => {
		const sessionId = 'ses-0123456789abcdef0123456789abcdef';
		const toolRunId = 'toolrun-016864084f6608895ba0e6ecb4ea39f8';
		let resolveRefresh!: (value: unknown) => void;
		const refreshResponse = new Promise((resolve) => {
			resolveRefresh = resolve;
		});
		vi.mocked(invoke)
			.mockReset()
			.mockReturnValueOnce(refreshResponse as never);

		const refresh = refreshToolRuns();
		upsertSessionToolRun({
			toolRunId,
			sessionId,
			kind: 'background',
			status: 'completed',
			output: 'event result',
		});
		resolveRefresh([
			{
				tool_run_id: toolRunId,
				kind: 'background',
				status: 'running',
			},
		]);
		await refresh;

		expect(get(sessionToolRunStore)[sessionId]?.[toolRunId]).toMatchObject({
			status: 'completed',
			output: 'event result',
		});
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('reconciles live rows across kinds while excluding terminal background history', async () => {
		resetToolRunStoreForTest();
		vi.mocked(invoke)
			.mockReset()
			.mockResolvedValue([
				{
					tool_run_id: 'toolrun-048f6d4ef95ee9b66749a96dce9776f6',
					kind: 'background',
					status: 'running',
				},
				{
					tool_run_id: 'toolrun-c65aaaf434059cb85b4c1924d843f10e',
					kind: 'background',
					status: 'completed',
				},
				{
					tool_run_id: 'toolrun-01a8a9fb492bc54b6940c29d266a1b9f',
					kind: 'scheduled',
					status: 'waiting',
				},
				{
					tool_run_id: 'toolrun-5f6561f3dba31459d605670145ba704f',
					kind: 'scheduled',
					status: 'running',
				},
			]);

		await refreshToolRuns();

		const rows = get(toolRunStore);
		expect(rows['toolrun-048f6d4ef95ee9b66749a96dce9776f6']?.status).toBe('running');
		expect(rows['toolrun-01a8a9fb492bc54b6940c29d266a1b9f']?.status).toBe('waiting');
		expect(rows['toolrun-5f6561f3dba31459d605670145ba704f']?.status).toBe('running');
		expect(rows['toolrun-c65aaaf434059cb85b4c1924d843f10e']).toBeUndefined();
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('finalizeBackgroundToolRunMessages clears toolRunId and writes terminal content', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			messages: [
				{
					id: 'msg-5a834705d100706f8a7f168139a48f64',
					toolRunId: 'toolrun-f0e73c4a1171ba7ddc9c666d937f82ec',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-f0e73c4a1171ba7ddc9c666d937f82ec',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});
		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-f0e73c4a1171ba7ddc9c666d937f82ec',
			kind: 'background',
			status: 'cancelled',
			output: 'stopped',
		});
		const msg = appSessionReducer.getMessages(
			'ses-0ecf371384ab79d597760ca41f2ba21c',
		)[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBeNull();
		expect(msg.sourceToolRunId).toBe('toolrun-f0e73c4a1171ba7ddc9c666d937f82ec');
		expect(msg.streaming).toBe(false);
		const body = JSON.parse(String(msg.content));
		expect(body.status).toBe('cancelled');
		expect(body.output).toBe('stopped');
	});

	it('does not project scheduled completion into a background tool card', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			messages: [
				{
					id: 'msg-0d3d0197b18f9a1b56d61fafda5d9ba4',
					toolRunId: 'toolrun-13a82ce6d9ad77004c1d1c01df4c9a4c',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-13a82ce6d9ad77004c1d1c01df4c9a4c',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});

		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-13a82ce6d9ad77004c1d1c01df4c9a4c',
			kind: 'scheduled',
			status: 'completed',
		});

		const msg = appSessionReducer.getMessages(
			'ses-0ecf371384ab79d597760ca41f2ba21c',
		)[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBe('toolrun-13a82ce6d9ad77004c1d1c01df4c9a4c');
		expect(msg.streaming).toBe(true);
		expect(JSON.parse(String(msg.content))).toMatchObject({ status: 'running' });
	});
});

describe('session ToolRun history hydration', () => {
	beforeEach(() => {
		resetToolRunStoreForTest();
		setActiveSessionToolRun(null);
		vi.mocked(invoke).mockReset();
	});

	it('loads terminal rows for the selected session after switching or restart', async () => {
		vi.mocked(invoke).mockResolvedValue([
			{
				tool_run_id: 'toolrun-6333ff0482315d2e6d91694e64d7be54',
				kind: 'scheduled',
				status: 'completed',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				title: 'Reminder',
			},
			{
				tool_run_id: 'toolrun-91afdc0999e04c5438155a1820d8efbb',
				kind: 'background',
				status: 'failed',
				session_id: 'ses-07a8107a71b8033314d1f83b5d330769',
			},
		]);

		await refreshSessionToolRuns('ses-0ecf371384ab79d597760ca41f2ba21c');

		expect(invoke).toHaveBeenCalledWith('list_tool_run_history', {
			kind: null,
			limit: 200,
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
		});
		expect(get(sessionToolRunStore)['ses-0ecf371384ab79d597760ca41f2ba21c']).toMatchObject({
			'toolrun-6333ff0482315d2e6d91694e64d7be54': { status: 'completed', kind: 'scheduled' },
		});
		expect(
			get(sessionToolRunStore)['ses-0ecf371384ab79d597760ca41f2ba21c']?.[
				'toolrun-91afdc0999e04c5438155a1820d8efbb'
			],
		).toBeUndefined();
		expect(get(sessionToolRunStore)['ses-07a8107a71b8033314d1f83b5d330769']).toBeUndefined();
	});

	it('keeps a newer lifecycle event received while history is loading', async () => {
		let resolveHistory!: (value: unknown) => void;
		const history = new Promise((resolve) => {
			resolveHistory = resolve;
		});
		vi.mocked(invoke).mockReturnValueOnce(history as never);

		const refresh = refreshSessionToolRuns('ses-0ecf371384ab79d597760ca41f2ba21c');
		upsertSessionToolRun({
			toolRunId: 'toolrun-016864084f6608895ba0e6ecb4ea39f8',
			kind: 'background',
			status: 'failed',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			output: 'new event result',
		});
		resolveHistory([
			{
				tool_run_id: 'toolrun-016864084f6608895ba0e6ecb4ea39f8',
				kind: 'background',
				status: 'completed',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				output: 'stale history result',
			},
		]);
		await refresh;

		expect(
			get(sessionToolRunStore)['ses-0ecf371384ab79d597760ca41f2ba21c']?.[
				'toolrun-016864084f6608895ba0e6ecb4ea39f8'
			],
		).toMatchObject({
			status: 'failed',
			output: 'new event result',
		});
	});

	it('bounds the cached ToolRun rows and number of cached sessions', () => {
		for (let index = 0; index < 205; index++) {
			upsertSessionToolRun({
				toolRunId: `toolrun-${(20_000 + index).toString(16).padStart(32, '0')}`,
				kind: 'scheduled',
				status: 'completed',
				sessionId: 'ses-93506352f1057381a474df372af1d8f8',
				finishedAt: new Date(1_700_000_000_000 + index * 1000).toISOString(),
			});
		}
		setActiveSessionToolRun('ses-93506352f1057381a474df372af1d8f8');
		expect(
			Object.keys(get(sessionToolRunStore)['ses-93506352f1057381a474df372af1d8f8'] || {}),
		).toHaveLength(200);
		for (let index = 0; index < 17; index++) {
			upsertSessionToolRun({
				toolRunId: `toolrun-${(40_000 + index).toString(16).padStart(32, '0')}`,
				kind: 'scheduled',
				sessionId: `ses-${(30_000 + index).toString(16).padStart(32, '0')}`,
			});
		}

		const store = get(sessionToolRunStore);
		expect(Object.keys(store)).toHaveLength(16);
		expect(Object.keys(store['ses-93506352f1057381a474df372af1d8f8'] || {})).toHaveLength(200);
		expect(
			Object.keys(store[`ses-${(30_000 + 16).toString(16).padStart(32, '0')}`] || {}),
		).toHaveLength(1);
	});
});
