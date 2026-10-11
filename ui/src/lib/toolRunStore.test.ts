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
			toolRunId: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			kind: 'background',
			status: 'running',
			startedAt: '2026-01-01T00:00:00Z',
		});
		const row = get(toolRunStore)['toolrun-6b86b273ff34fce19d6b804eff5a3f57'];
		expect(row.status).toBe('running');
		expect(row.kind).toBe('background');
		expect(row.toolRunId).toBe('toolrun-6b86b273ff34fce19d6b804eff5a3f57');
	});

	it('keeps a session binding update after an evicted row', () => {
		upsertToolRun({
			toolRunId: 'toolrun-ead6ef03d61ee60c533d6d450c50a1e5',
			kind: 'background',
			status: 'completed',
		});
		removeToolRun('toolrun-ead6ef03d61ee60c533d6d450c50a1e5');
		upsertToolRun({
			toolRunId: 'toolrun-ead6ef03d61ee60c533d6d450c50a1e5',
			kind: 'background',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
		});
		const row = get(toolRunStore)['toolrun-ead6ef03d61ee60c533d6d450c50a1e5'];
		expect(row.status).toBeUndefined();
		expect(row.sessionId).toBe('ses-6b86b273ff34fce19d6b804eff5a3f57');
	});

	it('keeps an explicit terminal status from tool_run:finished', () => {
		upsertToolRun({
			toolRunId: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
			kind: 'background',
			status: 'running',
		});
		upsertToolRun({
			toolRunId: 'toolrun-d4735e3a265e16eee03f59718b9b5d03',
			kind: 'background',
			status: 'completed',
			output: 'done',
		});
		const row = get(toolRunStore)['toolrun-d4735e3a265e16eee03f59718b9b5d03'];
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
		upsertToolRun({
			toolRunId: 'toolrun-247610f4dedd4ab7247d07dbda19c81c',
			kind: 'background',
			status: 'running',
		});
		const store = get(toolRunStore);
		expect(store['toolrun-247610f4dedd4ab7247d07dbda19c81c']?.status).toBe('running');
		expect(Object.keys(store).length).toBeLessThanOrEqual(64);
	});

	it('removeToolRun drops the row', () => {
		upsertToolRun({
			toolRunId: 'toolrun-4e07408562bedb8b60ce05c1decfe3ad',
			kind: 'background',
			status: 'running',
		});
		removeToolRun('toolrun-4e07408562bedb8b60ce05c1decfe3ad');
		expect(get(toolRunStore)['toolrun-4e07408562bedb8b60ce05c1decfe3ad']).toBeUndefined();
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
		resolveNewer([
			{
				tool_run_id: 'toolrun-11507a0e2f5e69d5dfa40a62a1bd7b6e',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await newerRefresh;
		resolveOlder([
			{
				tool_run_id: 'toolrun-cba06b5736faf67e54b07b561eae9439',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await olderRefresh;

		expect(get(toolRunStore)['toolrun-11507a0e2f5e69d5dfa40a62a1bd7b6e']?.status).toBe(
			'waiting',
		);
		expect(get(toolRunStore)['toolrun-cba06b5736faf67e54b07b561eae9439']).toBeUndefined();
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
			toolRunId: 'toolrun-129ce50dd90bf244858763d3f10932a9',
			kind: 'scheduled',
			status: 'running',
		});
		resolveRefresh([
			{
				tool_run_id: 'toolrun-129ce50dd90bf244858763d3f10932a9',
				kind: 'scheduled',
				status: 'waiting',
			},
		]);
		await refresh;

		expect(get(toolRunStore)['toolrun-129ce50dd90bf244858763d3f10932a9']?.status).toBe(
			'running',
		);
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('reconciles live rows across kinds while excluding terminal background history', async () => {
		toolRunStore.set({});
		vi.mocked(invoke)
			.mockReset()
			.mockResolvedValue([
				{
					tool_run_id: 'toolrun-53f0f736e59579919b9fef0854b7d696',
					kind: 'background',
					status: 'running',
				},
				{
					tool_run_id: 'toolrun-01e3c99eb1d506edf998a8190fcd2cd7',
					kind: 'background',
					status: 'completed',
				},
				{
					tool_run_id: 'toolrun-bbe41c5af12743e88a4c2e7bb3ef17c4',
					kind: 'scheduled',
					status: 'waiting',
				},
				{
					tool_run_id: 'toolrun-fa026afbc0da04f1e3a0bb550b62db4a',
					kind: 'scheduled',
					status: 'running',
				},
			]);

		await refreshToolRuns();

		const rows = get(toolRunStore);
		expect(rows['toolrun-53f0f736e59579919b9fef0854b7d696']?.status).toBe('running');
		expect(rows['toolrun-bbe41c5af12743e88a4c2e7bb3ef17c4']?.status).toBe('waiting');
		expect(rows['toolrun-fa026afbc0da04f1e3a0bb550b62db4a']?.status).toBe('running');
		expect(rows['toolrun-01e3c99eb1d506edf998a8190fcd2cd7']).toBeUndefined();
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('finalizeBackgroundToolRunMessages clears toolRunId and writes terminal content', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			messages: [
				{
					id: 'msg-6b86b273ff34fce19d6b804eff5a3f57',
					toolRunId: 'toolrun-a383bcb77c832a8ff52caace0f4efa8f',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-a383bcb77c832a8ff52caace0f4efa8f',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});
		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-a383bcb77c832a8ff52caace0f4efa8f',
			kind: 'background',
			status: 'cancelled',
			output: 'stopped',
		});
		const msg = appSessionReducer.getMessages(
			'ses-6b86b273ff34fce19d6b804eff5a3f57',
		)[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBeNull();
		expect(msg.sourceToolRunId).toBe('toolrun-a383bcb77c832a8ff52caace0f4efa8f');
		expect(msg.streaming).toBe(false);
		const body = JSON.parse(String(msg.content));
		expect(body.status).toBe('cancelled');
		expect(body.output).toBe('stopped');
	});

	it('does not project scheduled completion into a background tool card', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			messages: [
				{
					id: 'msg-d4735e3a265e16eee03f59718b9b5d03',
					toolRunId: 'toolrun-18beb67f31f2536585c6a34c733848fd',
					content: JSON.stringify({
						background: true,
						tool_run_id: 'toolrun-18beb67f31f2536585c6a34c733848fd',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});

		finalizeBackgroundToolRunMessages({
			toolRunId: 'toolrun-18beb67f31f2536585c6a34c733848fd',
			kind: 'scheduled',
			status: 'completed',
		});

		const msg = appSessionReducer.getMessages(
			'ses-6b86b273ff34fce19d6b804eff5a3f57',
		)[0] as unknown as Record<string, unknown>;
		expect(msg.toolRunId).toBe('toolrun-18beb67f31f2536585c6a34c733848fd');
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
				tool_run_id: 'toolrun-84097828fc31a8c8d29210df48901a85',
				kind: 'scheduled',
				status: 'completed',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				title: 'Reminder',
			},
			{
				tool_run_id: 'toolrun-5f711b23ee4fecbbb8e053b872bbda82',
				kind: 'background',
				status: 'failed',
				session_id: 'ses-d4735e3a265e16eee03f59718b9b5d03',
			},
		]);

		await refreshSessionToolRuns('ses-6b86b273ff34fce19d6b804eff5a3f57');

		expect(invoke).toHaveBeenCalledWith('list_tool_run_history', {
			kind: null,
			limit: 200,
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
		});
		expect(get(sessionToolRunStore)['ses-6b86b273ff34fce19d6b804eff5a3f57']).toMatchObject({
			'toolrun-84097828fc31a8c8d29210df48901a85': { status: 'completed', kind: 'scheduled' },
		});
		expect(
			get(sessionToolRunStore)['ses-6b86b273ff34fce19d6b804eff5a3f57']?.[
				'toolrun-5f711b23ee4fecbbb8e053b872bbda82'
			],
		).toBeUndefined();
		expect(get(sessionToolRunStore)['ses-d4735e3a265e16eee03f59718b9b5d03']).toBeUndefined();
	});

	it('keeps a newer lifecycle event received while history is loading', async () => {
		let resolveHistory!: (value: unknown) => void;
		const history = new Promise((resolve) => {
			resolveHistory = resolve;
		});
		vi.mocked(invoke).mockReturnValueOnce(history as never);

		const refresh = refreshSessionToolRuns('ses-6b86b273ff34fce19d6b804eff5a3f57');
		upsertSessionToolRun({
			toolRunId: 'toolrun-129ce50dd90bf244858763d3f10932a9',
			kind: 'background',
			status: 'failed',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			output: 'new event result',
		});
		resolveHistory([
			{
				tool_run_id: 'toolrun-129ce50dd90bf244858763d3f10932a9',
				kind: 'background',
				status: 'completed',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				output: 'stale history result',
			},
		]);
		await refresh;

		expect(
			get(sessionToolRunStore)['ses-6b86b273ff34fce19d6b804eff5a3f57']?.[
				'toolrun-129ce50dd90bf244858763d3f10932a9'
			],
		).toMatchObject({
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
				sessionId: 'ses-ec21b3b973a3a0e7050392031862848f',
				finishedAt: new Date(1_700_000_000_000 + index * 1000).toISOString(),
			});
		}
		setActiveSessionToolRun('ses-ec21b3b973a3a0e7050392031862848f');
		expect(
			Object.keys(get(sessionToolRunStore)['ses-ec21b3b973a3a0e7050392031862848f'] || {}),
		).toHaveLength(200);
		for (let index = 0; index < 17; index++) {
			upsertSessionToolRun({
				toolRunId: `toolrun-session-${index}`,
				kind: 'scheduled',
				sessionId: `ses-capacity-${index}`,
			});
		}

		const store = get(sessionToolRunStore);
		expect(Object.keys(store)).toHaveLength(16);
		expect(Object.keys(store['ses-ec21b3b973a3a0e7050392031862848f'] || {})).toHaveLength(200);
		expect(Object.keys(store['ses-capacity-16'] || {})).toHaveLength(1);
	});
});
