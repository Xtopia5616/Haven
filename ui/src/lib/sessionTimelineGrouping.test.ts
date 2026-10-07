import { describe, expect, it } from 'vitest';
import {
	groupSessionMessages,
	groupSessionTimeline,
	firstWaitingBackgroundToolRunId,
	isMergedSessionMessage,
} from './sessionTimeline.ts';
import type { SessionMessage } from './sessionReducer.ts';

const message = (id: string, extra: Partial<SessionMessage> = {}): SessionMessage => ({
	id,
	...extra,
});

describe('sessionTimeline grouping', () => {
	it('merges adjacent agent work and keeps user-facing messages separate', () => {
		const items = groupSessionMessages([
			message('user-1', { type: null }),
			message('thought-1', { type: 'thought', stepNumber: 1 }),
			message('tool-1', { type: 'tool', toolName: 'files', stepNumber: 1 }),
			message('reasoning-1', { type: 'reasoning', stepNumber: 2 }),
			message('answer-1', { type: null }),
		]);

		expect(items).toHaveLength(3);
		expect(items[0]).toMatchObject({ kind: 'message', message: { id: 'user-1' } });
		expect(items[1]).toMatchObject({
			kind: 'activity',
			id: 'activity-user-1-step-1',
			streaming: false,
			toolCount: 1,
			stepCount: 2,
		});
		if (items[1].kind === 'activity') {
			expect(items[1].entries.map(({ message: entry }) => entry.id)).toEqual([
				'thought-1',
				'tool-1',
				'reasoning-1',
			]);
		}
		expect(items[2]).toMatchObject({ kind: 'message', message: { id: 'answer-1' } });
	});

	it('keeps a work group active while any entry is streaming', () => {
		const items = groupSessionMessages([
			message('thought-1', { type: 'thought', streaming: false }),
			message('tool-1', { type: 'tool', streaming: true }),
		]);

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({ kind: 'activity', streaming: true, toolCount: 1 });
	});

	it('does not hide ask cards inside a work group', () => {
		const items = groupSessionMessages([
			message('tool-1', { type: 'tool' }),
			message('ask-1', { type: 'ask' }),
			message('tool-2', { type: 'tool' }),
		]);

		expect(items.map((item) => item.kind)).toEqual(['activity', 'message', 'activity']);
		expect(items[1]).toMatchObject({ kind: 'message', message: { id: 'ask-1' } });
	});

	it('only merges thought, reasoning and tool messages', () => {
		expect(isMergedSessionMessage(message('thought', { type: 'thought' }))).toBe(true);
		expect(isMergedSessionMessage(message('reasoning', { type: 'reasoning' }))).toBe(true);
		expect(isMergedSessionMessage(message('tool', { type: 'tool' }))).toBe(true);
		expect(isMergedSessionMessage(message('ask', { type: 'ask' }))).toBe(false);
		expect(isMergedSessionMessage(message('text', { type: null }))).toBe(false);
	});

	it('projects background status into its tool call and anchors scheduled cards', () => {
		const messages = [
			message('tool-background', {
				type: 'tool',
				stepNumber: 3,
				content: '{"execution_mode":"background","tool_run_id":"toolrun-background"}',
			}),
			message('tool-schedule', {
				type: 'tool',
				stepNumber: 4,
				toolName: 'schedule.set',
				content: '{"operation":"set","tool_run_id":"toolrun-scheduled"}',
			}),
		];
		const toolRuns = [
			{
				toolRunId: 'toolrun-background',
				kind: 'background' as const,
				status: 'running' as const,
				sessionId: 'ses-1',
			},
			{
				toolRunId: 'toolrun-scheduled',
				kind: 'scheduled' as const,
				status: 'waiting' as const,
				sessionId: 'ses-1',
			},
		];
		const items = groupSessionTimeline(messages, {
			toolRuns,
			awaitingBackground: true,
			awaitingBackgroundCount: 1,
		});

		expect(items.map((item) => item.kind)).toEqual(['activity', 'tool_run']);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [
				{ message: { id: 'tool-background' } },
				{ message: { id: 'tool-schedule' } },
			],
		});
		expect(items[1]).toMatchObject({
			kind: 'tool_run',
			toolRun: { toolRunId: 'toolrun-scheduled', kind: 'scheduled' },
			awaitingBackgroundResult: false,
		});
	});

	it('prefers sourceStepId over observation lookup', () => {
		const messages = [
			message('observation-anchor', {
				type: 'tool',
				stepNumber: 2,
				content: '{"execution_mode":"background","tool_run_id":"toolrun-source"}',
			}),
			message('message-boundary'),
			message('stable-source-step', {
				type: 'tool',
				stepNumber: 3,
				content: 'background task result unavailable',
			}),
		];
		const items = groupSessionTimeline(messages, {
			toolRuns: [{
				toolRunId: 'toolrun-source',
				kind: 'background',
				sourceStepId: 'stable-source-step',
			}],
		});

		expect(items.map((item) => item.kind)).toEqual(['activity', 'message', 'activity']);
		expect(items[2]).toMatchObject({ kind: 'activity', entries: [{ message: { id: 'stable-source-step' } }] });
	});

	it('keeps unanchored toolRuns in the owning session timeline and emits one wait fallback', () => {
		const toolRun = {
			toolRunId: 'toolrun-unanchored',
			kind: 'background' as const,
			status: 'running' as const,
			sessionId: 'ses-1',
		};
		const items = groupSessionTimeline([message('user-1')], {
			toolRuns: [toolRun],
			awaitingBackground: true,
			awaitingBackgroundCount: 0,
		});

		expect(items.map((item) => item.kind)).toEqual(['message', 'tool_run']);
		expect(items[1]).toMatchObject({
			kind: 'tool_run',
			toolRun: { toolRunId: 'toolrun-unanchored' },
			awaitingBackgroundResult: true,
		});

		const fallback = groupSessionTimeline([], { awaitingBackground: true });
		expect(fallback).toEqual([
			{
				kind: 'tool_run_wait',
				id: 'awaiting-background-result',
				awaitingBackgroundCount: 0,
			},
		]);
	});

	it('chooses the same earliest running background toolRun for the wait indicator', () => {
		expect(
			firstWaitingBackgroundToolRunId(
				[
					{ toolRunId: 'toolrun-later', kind: 'background', status: 'running', startedAt: '2026-10-06T11:00:00Z' },
					{ toolRunId: 'toolrun-scheduled', kind: 'scheduled', status: 'waiting' },
					{ toolRunId: 'toolrun-earlier', kind: 'background', status: 'running', startedAt: '2026-10-06T10:00:00Z' },
				],
				true,
			),
		).toBe('toolrun-earlier');
		expect(firstWaitingBackgroundToolRunId([], false)).toBeNull();
	});

	it('folds a terminal background result into its source tool card without a duplicate ToolRun card', () => {
		const toolRun = {
			toolRunId: 'toolrun-finished',
			kind: 'background' as const,
			status: 'completed' as const,
			sessionId: 'ses-1',
			output: 'same result',
		};
		const transcriptResult = message('tool-finished', {
			type: 'tool',
			toolRunId: null,
			sourceToolRunId: toolRun.toolRunId,
			content: JSON.stringify({
				execution_mode: 'background',
				tool_run_id: toolRun.toolRunId,
				status: 'completed',
				output: toolRun.output,
			}),
		});
		const items = groupSessionTimeline([transcriptResult], { toolRuns: [toolRun] });

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [{ message: { id: 'tool-finished', sourceToolRunId: toolRun.toolRunId } }],
		});
	});

	it('keeps terminal detail on the source tool card until the transcript receives it', () => {
		const toolRun = {
			toolRunId: 'toolrun-unprojected',
			kind: 'background' as const,
			status: 'failed' as const,
			sessionId: 'ses-1',
			error: 'process failed',
		};
		const source = message('tool-unprojected', {
			type: 'tool',
			toolRunId: toolRun.toolRunId,
			sourceToolRunId: toolRun.toolRunId,
			content: '{"background":true,"status":"running"}',
		});
		const items = groupSessionTimeline([source], { toolRuns: [toolRun] });

		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({
			kind: 'activity',
			entries: [{ message: { id: 'tool-unprojected', toolRunId: toolRun.toolRunId } }],
		});
	});
});
