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
} from './toolRunStore.ts';

describe('upsertToolRun', () => {
	beforeEach(() => {
		toolRunStore.set({});
	});

	it('keeps the explicit background create payload', () => {
		upsertToolRun({
			toolRunId: 'toolrun-1',
			kind: 'background',
			status: 'running',
			startedAt: '2026-01-01T00:00:00Z',
		});
		const row = get(toolRunStore)['toolrun-1'];
		expect(row.status).toBe('running');
		expect(row.kind).toBe('background');
		expect(row.toolRunId).toBe('toolrun-1');
	});

	it('keeps a session binding update after an evicted row', () => {
		upsertToolRun({ toolRunId: 'toolrun-ghost', kind: 'background', status: 'completed' });
		removeToolRun('toolrun-ghost');
		upsertToolRun({ toolRunId: 'toolrun-ghost', kind: 'background', sessionId: 'ses-1' });
		const row = get(toolRunStore)['toolrun-ghost'];
		expect(row.status).toBeUndefined();
		expect(row.sessionId).toBe('ses-1');
	});

	it('keeps an explicit terminal status from tool_run:finished', () => {
		upsertToolRun({ toolRunId: 'toolrun-2', kind: 'background', status: 'running' });
		upsertToolRun({
			toolRunId: 'toolrun-2',
			kind: 'background',
			status: 'completed',
			output: 'done',
		});
		const row = get(toolRunStore)['toolrun-2'];
		expect(row.status).toBe('completed');
		expect(row.output).toBe('done');
	});

	it('evicts terminal rows before live running rows when over capacity', () => {
		for (let i = 0; i < 70; i++) {
			upsertToolRun({
				toolRunId: `toolrun-done-${i}`,
				kind: 'background',
				status: 'completed',
			});
		}
		upsertToolRun({ toolRunId: 'toolrun-live', kind: 'background', status: 'running' });
		const store = get(toolRunStore);
		expect(store['toolrun-live']?.status).toBe('running');
		expect(Object.keys(store).length).toBeLessThanOrEqual(64);
	});

	it('removeToolRun drops the row', () => {
		upsertToolRun({ toolRunId: 'toolrun-3', kind: 'background', status: 'running' });
		removeToolRun('toolrun-3');
		expect(get(toolRunStore)['toolrun-3']).toBeUndefined();
	});

	it('ignores an older ToolRun refresh response', async () => {
		toolRunStore.set({});
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
		resolveNewer([{ tool_run_id: 'toolrun-new', kind: 'scheduled', status: 'waiting' }]);
		await newerRefresh;
		resolveOlder([{ tool_run_id: 'toolrun-old', kind: 'scheduled', status: 'waiting' }]);
		await olderRefresh;

		expect(get(toolRunStore)['toolrun-new']?.status).toBe('waiting');
		expect(get(toolRunStore)['toolrun-old']).toBeUndefined();
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
		upsertToolRun({ toolRunId: 'toolrun-race', kind: 'scheduled', status: 'running' });
		resolveRefresh([{ tool_run_id: 'toolrun-race', kind: 'scheduled', status: 'waiting' }]);
		await refresh;

		expect(get(toolRunStore)['toolrun-race']?.status).toBe('running');
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('reconciles live rows across kinds while excluding terminal background history', async () => {
		toolRunStore.set({});
		vi.mocked(invoke)
			.mockReset()
			.mockResolvedValue([
				{ tool_run_id: 'toolrun-background-live', kind: 'background', status: 'running' },
				{
					tool_run_id: 'toolrun-background-history',
					kind: 'background',
					status: 'completed',
				},
				{ tool_run_id: 'toolrun-scheduled-waiting', kind: 'scheduled', status: 'waiting' },
				{ tool_run_id: 'toolrun-scheduled-running', kind: 'scheduled', status: 'running' },
			]);

		await refreshToolRuns();

		const rows = get(toolRunStore);
		expect(rows['toolrun-background-live']?.status).toBe('running');
		expect(rows['toolrun-scheduled-waiting']?.status).toBe('waiting');
		expect(rows['toolrun-scheduled-running']?.status).toBe('running');
		expect(rows['toolrun-background-history']).toBeUndefined();
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('finalizeBackgroundToolRunMessages clears toolRunId and writes terminal content', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-1',
			messages: [
				{
					id: 'msg-1',
					toolRunId: 'toolrun-fin',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-fin',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});
		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-fin',
			kind: 'background',
			status: 'cancelled',
			output: 'stopped',
		});
		const msg = appSessionReducer.getMessages('ses-1')[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBeNull();
		expect(msg.sourceToolRunId).toBe('toolrun-fin');
		expect(msg.streaming).toBe(false);
		const body = JSON.parse(String(msg.content));
		expect(body.status).toBe('cancelled');
		expect(body.output).toBe('stopped');
	});

	it('does not project scheduled completion into a background tool card', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-1',
			messages: [
				{
					id: 'msg-2',
					toolRunId: 'toolrun-scheduled-fin',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-scheduled-fin',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});

		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-scheduled-fin',
			kind: 'scheduled',
			status: 'completed',
		});

		const msg = appSessionReducer.getMessages('ses-1')[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBe('toolrun-scheduled-fin');
		expect(msg.streaming).toBe(true);
		expect(JSON.parse(String(msg.content))).toMatchObject({ status: 'running' });
	});
});

describe('session ToolRun history hydration', () => {
	beforeEach(() => {
		sessionToolRunStore.set({});
		setActiveSessionToolRun(null);
		vi.mocked(invoke).mockReset();
	});

	it('loads terminal rows for the selected session after switching or restart', async () => {
		vi.mocked(invoke).mockResolvedValue([
			{
				tool_run_id: 'toolrun-session-1',
				kind: 'scheduled',
				status: 'completed',
				session_id: 'ses-1',
				title: 'Reminder',
			},
			{
				tool_run_id: 'toolrun-other-session',
				kind: 'background',
				status: 'failed',
				session_id: 'ses-2',
			},
		]);

		await refreshSessionToolRuns('ses-1');

		expect(invoke).toHaveBeenCalledWith('list_tool_run_history', {
			kind: null,
			limit: 200,
			sessionId: 'ses-1',
		});
		expect(get(sessionToolRunStore)['ses-1']).toMatchObject({
			'toolrun-session-1': { status: 'completed', kind: 'scheduled' },
		});
		expect(get(sessionToolRunStore)['ses-1']?.['toolrun-other-session']).toBeUndefined();
		expect(get(sessionToolRunStore)['ses-2']).toBeUndefined();
	});

	it('keeps a newer lifecycle event received while history is loading', async () => {
		let resolveHistory!: (value: unknown) => void;
		const history = new Promise((resolve) => {
			resolveHistory = resolve;
		});
		vi.mocked(invoke).mockReturnValueOnce(history as never);

		const refresh = refreshSessionToolRuns('ses-1');
		upsertSessionToolRun({
			toolRunId: 'toolrun-race',
			kind: 'background',
			status: 'failed',
			sessionId: 'ses-1',
			output: 'new event result',
		});
		resolveHistory([
			{
				tool_run_id: 'toolrun-race',
				kind: 'background',
				status: 'completed',
				session_id: 'ses-1',
				output: 'stale history result',
			},
		]);
		await refresh;

		expect(get(sessionToolRunStore)['ses-1']?.['toolrun-race']).toMatchObject({
			status: 'failed',
			output: 'new event result',
		});
	});

	it('bounds the cached ToolRun rows and number of cached sessions', () => {
		for (let index = 0; index < 205; index++) {
			upsertSessionToolRun({
				toolRunId: `toolrun-cap-${index}`,
				kind: 'scheduled',
				status: 'completed',
				sessionId: 'ses-capacity',
				finishedAt: new Date(1_700_000_000_000 + index * 1000).toISOString(),
			});
		}
		setActiveSessionToolRun('ses-capacity');
		expect(Object.keys(get(sessionToolRunStore)['ses-capacity'] || {})).toHaveLength(200);
		for (let index = 0; index < 17; index++) {
			upsertSessionToolRun({
				toolRunId: `toolrun-session-${index}`,
				kind: 'scheduled',
				sessionId: `ses-capacity-${index}`,
			});
		}

		const store = get(sessionToolRunStore);
		expect(Object.keys(store)).toHaveLength(16);
		expect(Object.keys(store['ses-capacity'] || {})).toHaveLength(200);
		expect(Object.keys(store['ses-capacity-16'] || {})).toHaveLength(1);
	});
});
