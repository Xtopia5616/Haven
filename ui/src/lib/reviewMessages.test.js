import { describe, it, expect } from 'vitest';
import { buildReviewMessages, mergeLiveStreaming } from './reviewMessages.js';
import { formatMessageTime } from './stores.js';

const sampleTask = {
	id: 'task-1',
	title: '打开记事本',
	input_text: '打开记事本',
	created_at: '2026-08-01T10:00:00.000Z',
};

describe('buildReviewMessages', () => {
	it('converts session messages into chat bubble items', () => {
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: '打开记事本', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: '已打开', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
			],
			steps: [],
		});
		expect(items).toHaveLength(2);
		expect(items[0]).toMatchObject({ id: 'm1', role: 'user', content: '打开记事本', streaming: false, voice: false });
		expect(items[1]).toMatchObject({ id: 'm2', role: 'assistant', content: '已打开' });
	});

	it('preserves the voice flag from persisted messages', () => {
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'mv', role: 'user', content: '打开计算器', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [], voice: true },
				{ id: 'mt', role: 'user', content: '打开记事本', message_type: 'text', created_at: '2026-08-01T10:02:00Z', attachments: [] },
			],
			steps: [],
		});
		expect(items[0]).toMatchObject({ id: 'mv', voice: true });
		expect(items[1]).toMatchObject({ id: 'mt', voice: false });
	});

	it('adds tool badges from steps with action_tool', () => {
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'hi', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
			],
			steps: [
				{ id: 's1', action_tool: 'file', observation: '{"ok":true}', thought: null, step_index: 1, created_at: '2026-08-01T10:01:00Z' },
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({ id: 'step-s1', type: 'tool', toolName: 'file', content: '{"ok":true}' });
	});

	it('hides silent tool steps like the live chat does', () => {
		// `"silent": true` on a tool input hides its card live; the review
		// rebuild must not resurrect it as a tool badge.
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'hi', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: '稍等', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
			],
			steps: [
				{ id: 's1', action_tool: 'shell', observation: '{"silent":true,"ok":true}', thought: null, silent: true, step_index: 1, created_at: '2026-08-01T10:01:00Z' },
			],
		});
		expect(items).toHaveLength(2);
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(0);
	});

	it('still assigns a stepNumber to the thought before a silent action step', () => {
		// The silent action itself has no badge, but its preceding thought
		// must resolve to the step via the matching thought step row so
		// rollback targeting keeps working.
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'hi', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: '稍等，我检查一下', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
				{ id: 'm3', role: 'assistant', content: '完成了', message_type: 'text', created_at: '2026-08-01T10:02:00Z', attachments: [] },
			],
			steps: [
				{ id: 't1', action_tool: null, thought: '稍等，我检查一下', silent: false, step_index: 1, created_at: '2026-08-01T10:01:00Z' },
				{ id: 's1', action_tool: 'shell', observation: 'ok', thought: null, silent: true, step_index: 1, created_at: '2026-08-01T10:01:01Z' },
			],
		});
		expect(items.find((i) => i.id === 'm2').stepNumber).toBe(1);
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(0);
	});

	it('falls back to the task input text when there are no messages', () => {
		const items = buildReviewMessages({ task: sampleTask, messages: [], steps: [] });
		expect(items).toHaveLength(1);
		expect(items[0]).toMatchObject({ role: 'user', content: '打开记事本' });
	});

	it('sorts items chronologically', () => {
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'late', role: 'assistant', content: 'later', message_type: 'text', created_at: '2026-08-01T10:05:00Z', attachments: [] },
				{ id: 'early', role: 'user', content: 'first', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
			],
			steps: [],
		});
		expect(items.map((i) => i.id)).toEqual(['early', 'late']);
	});

	it('renders an ask step as a dedup question card, not a raw tool badge', () => {
		// The ask tool persists the question BOTH as an assistant session
		// message and as a step observation. It must surface once, as an
		// `ask`-type card (no "Calling ask / Result" duplicate).
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'do it', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: '你想要怎么处理？A 还是 B？', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
			],
			steps: [
				{
					id: 's1',
					action_tool: 'ask',
					observation: '你想要怎么处理？A 还是 B？',
					thought: null,
					step_index: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({
			id: 'm2',
			type: 'ask',
			toolName: 'ask',
			content: '你想要怎么处理？A 还是 B？',
		});
		const toolBadges = items.filter((i) => i.type === 'tool');
		expect(toolBadges).toHaveLength(0);
	});

	it('extracts the question from a raw JSON ask observation for dedup', () => {
		// The DB stores the ask tool's structured output as raw JSON, while
		// the session message holds the readable question. The card must still
		// match and render as ask (not a raw JSON tool badge).
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'do it', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: '哪个文件？', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
			],
			steps: [
				{
					id: 's1',
					action_tool: 'ask',
					observation: JSON.stringify({ ask: true, question: '哪个文件？', context: null, awaiting_answer: true, hint: 'The task is paused.' }),
					thought: null,
					step_index: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({ id: 'm2', type: 'ask', content: '哪个文件？' });
		const toolBadges = items.filter((i) => i.type === 'tool');
		expect(toolBadges).toHaveLength(0);
	});

	it('renders a raw JSON ask observation as an ask card when no session message matches', () => {
		// Old tasks may lack the persisted session message for the question.
		// The step must still surface as an ask card with the extracted
		// question, never as a raw JSON tool badge.
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'go', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
			],
			steps: [
				{
					id: 's1',
					action_tool: 'ask',
					observation: JSON.stringify({ ask: true, question: '继续吗？', context: null, awaiting_answer: true, hint: 'The task is paused.' }),
					thought: null,
					step_index: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({
			id: 'step-s1',
			type: 'ask',
			toolName: 'ask',
			content: '继续吗？',
			options: [],
			awaiting: false,
		});
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(0);
	});

	it('dedups the first question when multiple ask calls are batched into one message', () => {
		// When the model batches two ask calls in one step, the persisted
		// assistant message joins the questions with "\n\n". Each step observes
		// only its own question. Both should still render as ask cards with no
		// raw tool badge or duplicated text.
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: 'go', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm2', role: 'assistant', content: 'Q1？\n\nQ2？', message_type: 'text', created_at: '2026-08-01T10:01:00Z', attachments: [] },
			],
			steps: [
				{ id: 's1', action_tool: 'ask', observation: 'Q1？', thought: null, step_index: 1, created_at: '2026-08-01T10:01:00Z' },
				{ id: 's2', action_tool: 'ask', observation: 'Q2？', thought: null, step_index: 2, created_at: '2026-08-01T10:01:01Z' },
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({ type: 'ask', content: 'Q1？\n\nQ2？' });
		expect(items.filter((i) => i.type === 'ask')).toHaveLength(1);
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(0);
	});

	it('matches an interrupted user message to its steering/supplement thought step', () => {
		// A message sent mid-generation (steering) or as an answer to a paused
		// task (supplement) is persisted as a thought step carrying the user's
		// own words. After reload the input must resolve to that step even
		// when nothing follows it (e.g. the task errored right after), so
		// rollback stays available.
		const items = buildReviewMessages({
			task: sampleTask,
			messages: [
				{ id: 'm1', role: 'user', content: '打开记事本', message_type: 'text', created_at: '2026-08-01T10:00:00Z', attachments: [] },
				{ id: 'm3', role: 'user', content: '网络不好就让我帮忙', message_type: 'text', created_at: '2026-08-01T10:02:00Z', attachments: [] },
			],
			steps: [
				{ id: 's2', action_tool: null, thought: '网络不好就让我帮忙', step_index: 2, created_at: '2026-08-01T10:02:00Z' },
			],
		});
		expect(items.find((i) => i.id === 'm3').stepNumber).toBe(2);
	});
});

describe('formatMessageTime', () => {
	it('formats non-today ISO timestamps as yyyy/mm/dd hh:mm:ss', () => {
		// Local-time ISO (no Z): the formatter renders in local time.
		expect(formatMessageTime('2026-08-01T10:05:09')).toBe('2026/08/01 10:05:09');
	});

	it('formats today timestamps as wall-clock time only', () => {
		// Same-day messages use the live-stream format so a merged review
		// list never mixes formats.
		const now = new Date();
		const today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 9, 30, 15);
		expect(formatMessageTime(today)).toBe(today.toLocaleTimeString());
	});
});

describe('mergeLiveStreaming', () => {
	const dbMessages = [
		{ id: 'm1', role: 'user', content: 'hi' },
		{ id: 'step-s1', type: 'tool', toolName: 'file', stepNumber: 1 },
	];

	it('merges DB messages with no streaming tail', () => {
		const merged = mergeLiveStreaming(dbMessages, []);
		expect(merged.map((m) => m.id)).toEqual(['m1', 'step-s1']);
	});

	it('appends streaming messages not already in the DB', () => {
		const existing = [
			{ id: 'tool-t-1-0-call1', type: 'tool', stepNumber: 2, streaming: true },
		];
		const merged = mergeLiveStreaming(dbMessages, existing);
		expect(merged.map((m) => m.id)).toEqual(['m1', 'step-s1', 'tool-t-1-0-call1']);
	});

	it('drops streaming messages whose id already exists in the DB', () => {
		const existing = [
			{ id: 'm1', streaming: true },
			{ id: 'tool-t-1-0-call1', streaming: true },
		];
		const merged = mergeLiveStreaming(dbMessages, existing);
		expect(merged.map((m) => m.id)).toEqual(['m1', 'step-s1', 'tool-t-1-0-call1']);
		// The DB copy wins; the duplicate streaming copy is not appended twice.
		expect(merged.filter((m) => m.id === 'm1')).toHaveLength(1);
	});

	it('drops DB tool-step badges already represented by a live tool card when dropToolSteps', () => {
		const existing = [
			{ id: 'tool-t-1-0-call1', type: 'tool', stepNumber: 1, streaming: true },
		];
		const merged = mergeLiveStreaming(dbMessages, existing, { dropToolSteps: true });
		// step-s1 (stepNumber 1) is dropped because a live card covers step 1.
		expect(merged.map((m) => m.id)).toEqual(['m1', 'tool-t-1-0-call1']);
	});

	it('keeps DB tool badges for steps with no live card even with dropToolSteps', () => {
		const existing = [
			{ id: 'tool-t-2-0-call2', type: 'tool', stepNumber: 2, streaming: true },
		];
		const merged = mergeLiveStreaming(dbMessages, existing, { dropToolSteps: true });
		expect(merged.map((m) => m.id)).toEqual(['m1', 'step-s1', 'tool-t-2-0-call2']);
	});
});
