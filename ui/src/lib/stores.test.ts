import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import {
	formatTokenCount,
	formatCostUsd,
	coalesceTokenTotal,
	cumulativeCacheHitRatePercent,
} from './sessionUsage.ts';
import { appSessionReducer } from './sessionReducer.ts';
import {
	notificationStore,
	addNotification,
	newMessage,
	modelStateStore,
	updateModelState,
	clearModelStateTimer,
	actionStore,
	upsertAction,
	removeAction,
	refreshActions,
	finalizeBackgroundActionMessages,
} from './stores.ts';

describe('upsertAction', () => {
	beforeEach(() => {
		actionStore.set({});
	});

	it('keeps the explicit background create payload', () => {
		upsertAction({
			id: 'act-1',
			kind: 'background',
			status: 'running',
			startedAt: '2026-01-01T00:00:00Z',
		});
		const row = get(actionStore)['act-1'];
		expect(row.status).toBe('running');
		expect(row.kind).toBe('background');
		expect(row.id).toBe('act-1');
	});

	it('keeps a session binding update after an evicted row', () => {
		upsertAction({ id: 'act-ghost', kind: 'background', status: 'completed' });
		removeAction('act-ghost');
		upsertAction({ id: 'act-ghost', kind: 'background', sessionId: 'ses-1' });
		const row = get(actionStore)['act-ghost'];
		expect(row.status).toBeUndefined();
		expect(row.sessionId).toBe('ses-1');
	});

	it('keeps an explicit terminal status from action:finished', () => {
		upsertAction({ id: 'act-2', kind: 'background', status: 'running' });
		upsertAction({ id: 'act-2', kind: 'background', status: 'completed', output: 'done' });
		const row = get(actionStore)['act-2'];
		expect(row.status).toBe('completed');
		expect(row.output).toBe('done');
	});

	it('evicts terminal rows before live running rows when over capacity', () => {
		for (let i = 0; i < 70; i++) {
			upsertAction({
				id: `act-done-${i}`,
				kind: 'background',
				status: 'completed',
			});
		}
		upsertAction({ id: 'act-live', kind: 'background', status: 'running' });
		const store = get(actionStore);
		expect(store['act-live']?.status).toBe('running');
		expect(Object.keys(store).length).toBeLessThanOrEqual(64);
	});

	it('removeAction drops the row', () => {
		upsertAction({ id: 'act-3', kind: 'background', status: 'running' });
		removeAction('act-3');
		expect(get(actionStore)['act-3']).toBeUndefined();
	});

	it('ignores an older action refresh response', async () => {
		actionStore.set({});
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
			.mockImplementationOnce(() => older)
			.mockImplementationOnce(() => newer);

		const olderRefresh = refreshActions();
		const newerRefresh = refreshActions();
		resolveNewer([{ id: 'act-new', kind: 'scheduled', status: 'waiting' }]);
		await newerRefresh;
		resolveOlder([{ id: 'act-old', kind: 'scheduled', status: 'waiting' }]);
		await olderRefresh;

		expect(get(actionStore)['act-new']?.status).toBe('waiting');
		expect(get(actionStore)['act-old']).toBeUndefined();
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('does not let a refresh overwrite a lifecycle event received in flight', async () => {
		let resolveRefresh!: (value: unknown) => void;
		const refreshResponse = new Promise((resolve) => {
			resolveRefresh = resolve;
		});
		vi.mocked(invoke).mockReset().mockReturnValueOnce(refreshResponse);

		const refresh = refreshActions();
		upsertAction({ id: 'act-race', kind: 'scheduled', status: 'running' });
		resolveRefresh([{ id: 'act-race', kind: 'scheduled', status: 'waiting' }]);
		await refresh;

		expect(get(actionStore)['act-race']?.status).toBe('running');
		vi.mocked(invoke).mockResolvedValue(undefined);
	});

	it('finalizeBackgroundActionMessages clears actionId and writes terminal content', () => {
		appSessionReducer.dispatch({ type: 'sessions/cleared' });
		appSessionReducer.dispatch({
			type: 'session/messages/resume-loaded',
			sessionId: 'ses-1',
			messages: [
				{
					id: 'msg-1',
					actionId: 'act-fin',
					content: JSON.stringify({
						background: true,
						action_id: 'act-fin',
						status: 'running',
					}),
					streaming: true,
				},
			],
		});
		finalizeBackgroundActionMessages({
			id: 'act-fin',
			kind: 'background',
			status: 'cancelled',
			output: 'stopped',
		});
		const msg = appSessionReducer.getMessages('ses-1')[0] as unknown as Record<string, unknown>;
		expect(msg.actionId).toBeNull();
		expect(msg.streaming).toBe(false);
		const body = JSON.parse(String(msg.content));
		expect(body.status).toBe('cancelled');
		expect(body.output).toBe('stopped');
	});
});

describe('addNotification', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		notificationStore.set([]);
		vi.mocked(invoke).mockClear();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('adds a notification with msg and type', () => {
		addNotification('hello', 'info');
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('hello');
		expect(items[0].type).toBe('info');
		expect(typeof items[0].id).toBe('string');
	});

	it('deduplicates identical msg+type', () => {
		addNotification('same', 'warning');
		addNotification('same', 'warning');
		addNotification('same', 'info');
		expect(get(notificationStore)).toHaveLength(2);
	});

	it('auto-removes after the duration', () => {
		addNotification('temp', 'info', 1000);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(999);
		expect(get(notificationStore)).toHaveLength(1);
		vi.advanceTimersByTime(2);
		expect(get(notificationStore)).toHaveLength(0);
	});

	it('removes only its own notification', () => {
		addNotification('a', 'info', 1000);
		addNotification('b', 'info', 5000);
		vi.advanceTimersByTime(1001);
		const items = get(notificationStore);
		expect(items).toHaveLength(1);
		expect(items[0].msg).toBe('b');
	});

	it('logs error notifications via logger.error', () => {
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		addNotification('boom', 'error');
		addNotification('ok', 'info');
		addNotification('oops', 'warning');
		const errorCalls = spy.mock.calls.filter(
			(args) => typeof args[0] === 'string' && args[0].includes('[ERROR]'),
		);
		expect(errorCalls).toHaveLength(1);
		expect(errorCalls[0][0]).toContain('notification');
		expect(errorCalls[0][0]).toContain('boom');
		spy.mockRestore();
	});

	it('mirrors error notifications to the backend log', () => {
		addNotification('回退失败: target message not found', 'error');

		expect(invoke).toHaveBeenCalledWith('log_frontend_error', {
			message: '回退失败: target message not found',
		});
	});

	it('does not mirror non-error notifications to the backend log', () => {
		addNotification('正在加载…', 'info');

		expect(invoke).not.toHaveBeenCalled();
	});
});

describe('newMessage', () => {
	it('builds a message with default type and voice', () => {
		const msg = newMessage({ role: 'assistant', content: 'hi' });
		expect(msg.role).toBe('assistant');
		expect(msg.content).toBe('hi');
		expect(msg.type).toBeNull();
		expect(msg.voice).toBe(false);
		expect(typeof msg.id).toBe('string');
		expect(msg.time).toBeTruthy();
	});

	it('generates unique ids', () => {
		const a = newMessage({ role: 'user', content: 'x' });
		const b = newMessage({ role: 'user', content: 'x' });
		expect(a.id).not.toBe(b.id);
	});

	it('idPrefix slots into the id between timestamp and randomness', () => {
		const msg = newMessage({ role: 'user', content: 'x', idPrefix: 'u' });
		expect(msg.id).toMatch(/^\d+-u-[a-z0-9]+$/);
	});

	it('keeps attachments and overrides time and voice', () => {
		const msg = newMessage({
			role: 'user',
			content: 'x',
			voice: true,
			time: '12:00:00',
			attachments: [{ media_type: 'image/png', data: 'a' }],
		});
		expect(msg.voice).toBe(true);
		expect(msg.time).toBe('12:00:00');
		expect(msg.attachments).toEqual([{ media_type: 'image/png', data: 'a' }]);
	});
});

describe('updateModelState', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		clearModelStateTimer();
		modelStateStore.set('ready');
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('falls back from waiting to ready after the default delay', () => {
		updateModelState('waiting');
		expect(get(modelStateStore)).toBe('waiting');
		vi.advanceTimersByTime(5000);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('falls back from streaming to ready after 2s', () => {
		updateModelState('streaming');
		vi.advanceTimersByTime(2000);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('honours a custom idle timeout', () => {
		updateModelState('waiting', { idleTimeoutMs: 100 });
		vi.advanceTimersByTime(100);
		expect(get(modelStateStore)).toBe('ready');
	});

	it('a new state supersedes the pending idle timer', () => {
		updateModelState('waiting');
		updateModelState('streaming');
		vi.advanceTimersByTime(2000);
		// Only the 2s streaming idle timer applies; the waiting timer was cleared.
		expect(get(modelStateStore)).toBe('ready');
	});

	it('clearModelStateTimer cancels a pending idle timer', () => {
		updateModelState('waiting');
		clearModelStateTimer();
		vi.advanceTimersByTime(10000);
		expect(get(modelStateStore)).toBe('waiting');
	});

	it('stalled persists until a later state supersedes it', () => {
		// Stall is a factual waiting state and must NOT auto-revert: the
		// provider may stay silent for the whole idle-timeout window; only
		// the next chunk (streaming) or a terminal session event clears it.
		updateModelState('stalled');
		vi.advanceTimersByTime(60000);
		expect(get(modelStateStore)).toBe('stalled');
		updateModelState('streaming');
		expect(get(modelStateStore)).toBe('streaming');
	});
});

describe('token usage helpers', () => {
	it('coalesceTokenTotal fills omitted total', () => {
		expect(coalesceTokenTotal(10, 5, 0)).toBe(15);
		expect(coalesceTokenTotal(10, 5, 20)).toBe(20);
		expect(coalesceTokenTotal(0, 0, 0)).toBe(0);
	});

	it('coalesceTokenTotal adds exclusive cache when total is omitted', () => {
		expect(coalesceTokenTotal(100, 20, 0, 400, 50, 'exclusive')).toBe(570);
		expect(coalesceTokenTotal(100, 20, 0, 80, 0)).toBe(120);
		expect(coalesceTokenTotal(100, 20, 125, 80, 0)).toBe(125);
	});

	it('calculates a mixed-provider cache rate from each call contract', () => {
		const rate = cumulativeCacheHitRatePercent([
			{
				call_kind: 'agent',
				prompt_tokens: 100,
				cached_tokens: 100,
				cache_accounting: 'inclusive',
			},
			{
				call_kind: 'agent',
				prompt_tokens: 100,
				cached_tokens: 400,
				cache_accounting: 'exclusive',
			},
		]);
		expect(rate).toBeCloseTo((500 / 600) * 100, 6);
	});

	it('does not guess a cache rate for unknown calls', () => {
		expect(
			cumulativeCacheHitRatePercent([
				{
					call_kind: 'agent',
					prompt_tokens: 100,
					cached_tokens: 80,
					cache_accounting: 'unknown',
				},
			]),
		).toBeNull();
	});

	it('excludes media-owned calls from the Agent cache rate', () => {
		expect(
			cumulativeCacheHitRatePercent([
				{
					call_kind: 'agent',
					prompt_tokens: 100,
					cached_tokens: 50,
					cache_accounting: 'inclusive',
				},
				{
					call_kind: 'media',
					prompt_tokens: 10_000,
					cached_tokens: 0,
					cache_accounting: 'unknown',
				},
			]),
		).toBe(50);
	});
});

describe('formatTokenCount', () => {
	it('formats plain counts without suffix', () => {
		expect(formatTokenCount(0)).toBe('0');
		expect(formatTokenCount(999)).toBe('999');
	});

	it('formats thousands with K suffix', () => {
		expect(formatTokenCount(1234)).toBe('1.23K');
		expect(formatTokenCount(12000)).toBe('12K');
	});

	it('formats millions with M suffix', () => {
		expect(formatTokenCount(1234567)).toBe('1.23M');
	});

	it('tolerates non-numeric input', () => {
		expect(formatTokenCount(undefined as any)).toBe('0');
		expect(formatTokenCount('300' as any)).toBe('300');
	});
});

describe('formatCostUsd', () => {
	it('returns null for missing or non-finite values', () => {
		expect(formatCostUsd(null)).toBeNull();
		expect(formatCostUsd(undefined)).toBeNull();
		expect(formatCostUsd(NaN)).toBeNull();
		expect(formatCostUsd(Infinity)).toBeNull();
	});

	it('formats zero', () => {
		expect(formatCostUsd(0)).toBe('$0.00');
	});

	it('uses 4 decimals for sub-cent costs', () => {
		expect(formatCostUsd(0.00123)).toBe('$0.0012');
	});

	it('uses 3 decimals under one dollar', () => {
		expect(formatCostUsd(0.1234)).toBe('$0.123');
	});

	it('uses 2 decimals for whole dollars', () => {
		expect(formatCostUsd(1.5)).toBe('$1.50');
		expect(formatCostUsd(21)).toBe('$21.00');
	});
});
