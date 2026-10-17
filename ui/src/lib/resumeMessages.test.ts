import { describe, it, expect } from 'vitest';
import { buildResumeMessages, mergeLiveStreaming } from './resumeMessages.ts';
import { formatMessageTime } from './messageFormat.ts';

const sampleSession = {
	id: 'session-1',
	title: '打开记事本',
	input_text: '打开记事本',
	created_at: '2026-08-01T10:00:00.000Z',
};

describe('buildResumeMessages', () => {
	it('carries durable unknown tool outcomes into history cards', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [],
			steps: [
				{
					stepId: 'step-543950710a972d7bd47f45c9d9e0965c',
					tool_name: 'write_file',
					status: 'unknown',
					observation: 'may have run',
					created_at: '2026-09-07T00:00:00.000Z',
					step_number: 1,
				},
			],
		});
		expect(items).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ id: 'step-543950710a972d7bd47f45c9d9e0965c', type: 'tool', outcome: 'unknown' }),
			]),
		);
	});
	it('converts session messages into chat bubble items', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: '打开记事本',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '已打开',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [],
		});
		expect(items).toHaveLength(2);
		expect(items[0]).toMatchObject({
			id: 'msg-00000000000000000000000000000001',
			role: 'user' as const,
			content: '打开记事本',
			streaming: false,
			voice: false,
		});
		expect(items[1]).toMatchObject({ id: 'msg-00000000000000000000000000000002', role: 'assistant' as const, content: '已打开' });
	});

	it('preserves the voice flag from persisted messages', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'mv',
					role: 'user' as const,
					content: '打开计算器',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
					voice: true,
				},
				{
					messageId: 'mt',
					role: 'user' as const,
					content: '打开记事本',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [],
		});
		expect(items[0]).toMatchObject({ id: 'mv', voice: true });
		expect(items[1]).toMatchObject({ id: 'mt', voice: false });
	});

	it('maps durable message kinds to renderer presentation types', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'thought',
					role: 'assistant',
					content: 'thinking',
					message_type: 'thought',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'tool-call',
					role: 'assistant',
					content: 'calling',
					message_type: 'tool_call',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
				{
					messageId: 'reasoning',
					role: 'assistant',
					content: 'reasoning',
					message_type: 'reasoning',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
				{
					messageId: 'peer-kickoff',
					role: 'user',
					content: 'delegate',
					message_type: 'peer_kickoff',
					created_at: '2026-08-01T10:03:00Z',
					attachments: [],
				},
			],
			steps: [],
		});

		expect(items.map(({ id, type }) => [id, type])).toEqual([
			['thought', 'thought'],
			['tool-call', 'tool_call'],
			['reasoning', 'reasoning'],
			['peer-kickoff', 'peer_kickoff'],
		]);
	});

	it('adds tool badges from steps with tool_name', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'files',
					observation: '{"ok":true}',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({
			id: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
			type: 'tool',
			toolName: 'files',
			content: '{"ok":true}',
		});
	});

	it('restores operation-view steps by stable step identity, not thought text', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: '读取文件',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'm-thought',
					role: 'assistant' as const,
					content: '检查目标文件',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-cf3cabbf64676a92780eea6db66a1f3e',
					tool_name: 'files.read',
					tool_input: '{"path":"notes.md"}',
					observation: '{"operation":"read","path":"notes.md","content":"ok"}',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:01Z',
				},
			],
		});
		const tool = items.find((item) => item.id === 'step-cf3cabbf64676a92780eea6db66a1f3e');
		expect(tool).toMatchObject({
			id: 'step-cf3cabbf64676a92780eea6db66a1f3e',
			type: 'tool',
			toolName: 'files.read',
			toolArgs: '{"path":"notes.md"}',
		});
	});

	it('keeps persisted tool names verbatim', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [],
			steps: [
				{
					stepId: 'step-cf023ba77a3ea05387eb80133d4fb0e8',
					tool_name: 'file',
					observation: '{"content":"old"}',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
				{
					stepId: 'step-9b06cc05e7e3d240b08457c785accfc4',
					tool_name: 'file_search',
					observation: '{"results":[]}',
					thought: null,
					step_number: 2,
					created_at: '2026-08-01T10:02:00Z',
				},
				{
					stepId: 'step-052b341c6a7d077effc2e1976c4ebdc7',
					tool_name: 'scheduled_tool_run',
					observation: '{"scheduled_tool_runs":[]}',
					thought: null,
					step_number: 3,
					created_at: '2026-08-01T10:03:00Z',
				},
			],
		});
		expect(items.map((item) => item.toolName)).toEqual([
			'file',
			'file_search',
			'scheduled_tool_run',
		]);
	});

	it('normalizes persisted tool-role observations into tool cards', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: '检查',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'tool' as const,
					content: '{"output":"ok"}',
					message_type: 'observation',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'shell',
					tool_input: '{"cmd":"dir"}',
					observation: '{"output":"ok"}',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		const tool = items.find((item) => item.id === 'step-edfd6f6b7218b976ac1daf2a9709bba0');
		expect(tool).toMatchObject({
			role: 'assistant' as const,
			type: 'tool',
			toolName: 'shell',
			toolArgs: '{"cmd":"dir"}',
		});
	});

	it('carries tool_input onto tool cards as toolArgs', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'mcp__filesystem__read',
					tool_input: '{"path":"a.rs"}',
					observation: '{"ok":true}',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items[1]).toMatchObject({
			id: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
			type: 'tool',
			toolName: 'mcp__filesystem__read',
			toolArgs: '{"path":"a.rs"}',
			content: '{"ok":true}',
		});
	});

	it('restores silent-input tool steps with the same visible card as other tools', () => {
		// The input flag is retained as ordinary tool parameters so the resumed
		// card still shows status, parameters and output consistently.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '稍等',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 's1',
					tool_name: 'shell',
					tool_input: '{"command":"echo hi","silent":true}',
					observation: '{"silent":true,"ok":true}',
					thought: null,
					silent: true,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(3);
		expect(items.find((i) => i.type === 'tool')).toMatchObject({
			id: 's1',
			toolName: 'shell',
			toolArgs: '{"command":"echo hi","silent":true}',
			content: '{"silent":true,"ok":true}',
		});
	});

	it('assigns a stepNumber to the thought before a visible ToolCall step', () => {
		// The ToolCall card is visible too, while the preceding thought still
		// resolves to its step for rollback targeting.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '稍等，我检查一下',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000003',
					role: 'assistant' as const,
					content: '完成了',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 't1',
					tool_name: null,
					thought: '稍等，我检查一下',
					silent: false,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
				{
					stepId: 's1',
					tool_name: 'shell',
					observation: 'ok',
					thought: null,
					silent: true,
					step_number: 1,
					created_at: '2026-08-01T10:01:01Z',
				},
			],
		});
		expect(items.find((i) => i.id === 'msg-00000000000000000000000000000002')!.stepNumber).toBe(1);
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(1);
	});

	it('does not treat session input metadata as transcript when projections are empty', () => {
		const items = buildResumeMessages({ session: sampleSession, messages: [], steps: [] });
		expect(items).toEqual([]);
	});

	it('sorts items chronologically', () => {
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'late',
					role: 'assistant' as const,
					content: 'later',
					message_type: 'text',
					created_at: '2026-08-01T10:05:00Z',
					attachments: [],
				},
				{
					messageId: 'early',
					role: 'user' as const,
					content: 'first',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
			],
			steps: [],
		});
		expect(items.map((i) => i.id)).toEqual(['early', 'late']);
	});

	it('orders a tool card by completed_at so it cannot land after the answer it precedes', () => {
		// Regression: the step row's created_at (tool START) ties with the
		// PRECEDING thought's row (same second), and the stable `_ts` sort
		// kept "all messages before all steps" — the card was pushed below
		// the following answer. The card's logical position is when its
		// observation landed (completed_at), which sits between the thought
		// and the next message whenever the tool spans a second boundary.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '稍等，我查一下',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000003',
					role: 'assistant' as const,
					content: '完成了',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'shell',
					observation: 'ok',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
					completed_at: '2026-08-01T10:01:01Z',
				},
			],
		});
		expect(items.map((i) => i.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-00000000000000000000000000000002', 'step-edfd6f6b7218b976ac1daf2a9709bba0', 'msg-00000000000000000000000000000003']);
	});

	it('keeps a same-second tool card before its answer with ms timestamps', () => {
		// Millisecond-precision rows (the backend writes them since the
		// ordering fix): the answer row is persisted AFTER the tool
		// completed, so its created_at is later than the card's completed_at
		// even within one second — the card sorts between thought and answer.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '稍等',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00.100Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000003',
					role: 'assistant' as const,
					content: '完成了',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00.900Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'shell',
					observation: 'ok',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00.100Z',
					completed_at: '2026-08-01T10:01:00.500Z',
				},
			],
		});
		expect(items.map((i) => i.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-00000000000000000000000000000002', 'step-edfd6f6b7218b976ac1daf2a9709bba0', 'msg-00000000000000000000000000000003']);
	});

	it('falls back to created_at for cards without completed_at', () => {
		// Steps that never completed (interrupted/failed) have no
		// completed_at; the card keeps its creation time as the sort key.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000002',
					role: 'assistant' as const,
					content: '稍等',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'shell',
					observation: 'ok',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
					completed_at: null,
				},
			],
		});
		expect(items.map((i) => i.id)).toEqual(['msg-00000000000000000000000000000001', 'step-edfd6f6b7218b976ac1daf2a9709bba0', 'msg-00000000000000000000000000000002']);
	});

	it('renders an ask step as a dedup question card, not a raw tool badge', () => {
		// The ask tool persists the question message under the step id and
		// keeps the structured observation as metadata. It must surface once,
		// as an `ask`-type card under the step row's id — no
		// "Calling ask / Result" duplicate, no extra question bubble.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'do it',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'assistant' as const,
					content: '你想要怎么处理？A 还是 B？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'ask',
					observation: JSON.stringify({ ask: true }),
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({
			id: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
			type: 'ask',
			toolName: 'ask',
			content: '你想要怎么处理？A 还是 B？',
		});
		const toolBadges = items.filter((i) => i.type === 'tool');
		expect(toolBadges).toHaveLength(0);
		expect(items.filter((i) => i.id === 'step-edfd6f6b7218b976ac1daf2a9709bba0')).toHaveLength(1);
	});

	it('links an ask card to its question message persisted under the step id', () => {
		// New records persist ONE question message per ask step, under the
		// step row's id (the message is the card's content authority). The
		// resume build must skip the message by id and render the card from
		// it — no sentinel or content matching.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'do it',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'assistant' as const,
					content: '继续吗？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'ask',
					observation: JSON.stringify({
						ask: true,
						question: '继续吗？',
						options: ['A', 'B'],
					}),
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items[1]).toMatchObject({
			id: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
			type: 'ask',
			toolName: 'ask',
			content: '继续吗？',
			options: ['A', 'B'],
		});
		expect(items.filter((i) => i.id === 'step-edfd6f6b7218b976ac1daf2a9709bba0')).toHaveLength(1);
	});

	it('renders each ask of a batched step from its own id-shared message', () => {
		// The batched-ask case on new records: one message per ask step,
		// each under its step id — both cards render, each from its message.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'go',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'assistant' as const,
					content: 'Q1？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
				{
					messageId: 'step-5d310391cc7f4a1e5b7748d3f16e751a',
					role: 'assistant' as const,
					content: 'Q2？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:01Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'ask',
					observation: 'Q1？',
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
				{
					stepId: 'step-5d310391cc7f4a1e5b7748d3f16e751a',
					tool_name: 'ask',
					observation: 'Q2？',
					thought: null,
					step_number: 2,
					created_at: '2026-08-01T10:01:01Z',
				},
			],
		});
		expect(items).toHaveLength(3);
		expect(items.filter((i) => i.type === 'ask').map((i) => i.content)).toEqual([
			'Q1？',
			'Q2？',
		]);
		expect(items.filter((i) => i.type === 'tool')).toHaveLength(0);
	});

	it('resolves a thought stepNumber by id when the message shares the step id', () => {
		// New records persist the thought text only in messages, and the
		// thought step row under the SAME id — no content matching needed.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'hi',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-160d44dda6c0a478d9996206c6b3ec5e',
					role: 'assistant' as const,
					content: '稍等，我检查一下',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-160d44dda6c0a478d9996206c6b3ec5e',
					tool_name: null,
					thought: null,
					silent: false,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items).toHaveLength(2);
		expect(items.find((i) => i.id === 'step-160d44dda6c0a478d9996206c6b3ec5e')!.stepNumber).toBe(1);
	});

	it('resolves a supplement user message stepNumber by id', () => {
		// Steering/supplement inputs: the thought step row is created under
		// the user message's own id (no thought text), so the interrupted
		// input resolves to its step by id after reload.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: '打开记事本',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000003',
					role: 'user' as const,
					content: '网络不好就让我帮忙',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'msg-00000000000000000000000000000003',
					tool_name: null,
					thought: null,
					step_number: 2,
					created_at: '2026-08-01T10:02:00Z',
				},
			],
		});
		expect(items.find((i) => i.id === 'msg-00000000000000000000000000000003')!.stepNumber).toBe(2);
	});

	it('links an interrupted user message to its id-shared thought step', () => {
		// A message sent mid-generation (steering) or as an answer to a paused
		// session (supplement) is persisted as a thought step carrying the user's
		// own words. After reload the input must resolve to that step even
		// when nothing follows it (e.g. the session errored right after), so
		// rollback stays available.
		const items = buildResumeMessages({
			session: sampleSession,
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: '打开记事本',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'msg-00000000000000000000000000000003',
					role: 'user' as const,
					content: '网络不好就让我帮忙',
					message_type: 'text',
					created_at: '2026-08-01T10:02:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'msg-00000000000000000000000000000003',
					tool_name: null,
					thought: null,
					step_number: 2,
					created_at: '2026-08-01T10:02:00Z',
				},
			],
		});
		expect(items.find((i) => i.id === 'msg-00000000000000000000000000000003')!.stepNumber).toBe(2);
	});

	it('restores ask options without duplicating interaction state', () => {
		// Resume rebuilds the card content/options. Pending state is hydrated
		// separately from the safe interaction projection.
		const items = buildResumeMessages({
			session: { ...sampleSession, status: 'paused' },
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'go',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'assistant' as const,
					content: '继续吗？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'ask',
					observation: JSON.stringify({
						ask: true,
						question: '继续吗？',
						options: ['A', 'B'],
					}),
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items[1]).toMatchObject({
			id: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
			type: 'ask',
			content: '继续吗？',
			options: ['A', 'B'],
			awaiting: false,
		});
	});

	it('keeps ask cards non-awaiting for non-paused sessions', () => {
		const items = buildResumeMessages({
			session: { ...sampleSession, status: 'completed' },
			messages: [
				{
					messageId: 'msg-00000000000000000000000000000001',
					role: 'user' as const,
					content: 'go',
					message_type: 'text',
					created_at: '2026-08-01T10:00:00Z',
					attachments: [],
				},
				{
					messageId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					role: 'assistant' as const,
					content: '继续吗？',
					message_type: 'text',
					created_at: '2026-08-01T10:01:00Z',
					attachments: [],
				},
			],
			steps: [
				{
					stepId: 'step-edfd6f6b7218b976ac1daf2a9709bba0',
					tool_name: 'ask',
					observation: JSON.stringify({
						ask: true,
						question: '继续吗？',
						options: ['A', 'B'],
					}),
					thought: null,
					step_number: 1,
					created_at: '2026-08-01T10:01:00Z',
				},
			],
		});
		expect(items[1]).toMatchObject({ type: 'ask', options: ['A', 'B'], awaiting: false });
	});
});

describe('formatMessageTime', () => {
	it('formats non-today ISO timestamps as yyyy/mm/dd hh:mm:ss', () => {
		// Local-time ISO (no Z): the formatter renders in local time.
		expect(formatMessageTime('2026-08-01T10:05:09')).toBe('2026/08/01 10:05:09');
	});

	it('formats today timestamps as wall-clock time only', () => {
		// Same-day messages use the live-stream format so a merged resume
		// list never mixes formats.
		const now = new Date();
		const today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 9, 30, 15);
		expect(formatMessageTime(today)).toBe(today.toLocaleTimeString());
	});
});

describe('mergeLiveStreaming', () => {
	const dbMessages = [
		{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
		{ id: 'step-edfd6f6b7218b976ac1daf2a9709bba0', type: 'tool' as const, toolName: 'files', stepNumber: 1 },
	];

	it('merges DB messages with no streaming tail', () => {
		const merged = mergeLiveStreaming(dbMessages, []);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'step-edfd6f6b7218b976ac1daf2a9709bba0']);
	});

	it('appends streaming messages not already in the DB', () => {
		const existing = [{ id: 'step-5d310391cc7f4a1e5b7748d3f16e751a', type: 'tool' as const, stepNumber: 2, streaming: true }];
		const merged = mergeLiveStreaming(dbMessages, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'step-edfd6f6b7218b976ac1daf2a9709bba0', 'step-5d310391cc7f4a1e5b7748d3f16e751a']);
	});

	it('drops streaming messages whose id already exists in the DB', () => {
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', streaming: true },
			{ id: 'step-4626a4fa5daedbd40725050ffb1f4e74', type: 'tool' as const, streaming: true },
		];
		const merged = mergeLiveStreaming(dbMessages, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'step-edfd6f6b7218b976ac1daf2a9709bba0', 'step-4626a4fa5daedbd40725050ffb1f4e74']);
		// The DB copy wins; the duplicate streaming copy is not appended twice.
		expect(merged.filter((m) => m.id === 'msg-00000000000000000000000000000001')).toHaveLength(1);
	});

	it('keeps a single tool card when the live card and the DB badge share the step id', () => {
		// The live tool card and the DB step badge are the SAME entity now
		// (both keyed by the backend-minted `step-*` id), so the merge can
		// never show a card plus a badge for one step.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'step-00000000000000000000000000000001',
				type: 'tool' as const,
				toolName: 'files',
				stepNumber: 1,
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'step-00000000000000000000000000000001',
				type: 'tool' as const,
				toolName: 'files',
				stepNumber: 1,
				streaming: true,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'step-00000000000000000000000000000001']);
		expect(merged.filter((m) => m.id === 'step-00000000000000000000000000000001')).toHaveLength(1);
	});

	it('dedups finalized live reasoning against the DB copy by id', () => {
		// The live reasoning bubble and the persisted reasoning row share the
		// minted message id, so the DB copy simply replaces the live one.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'msg-00000000000000000000000000000009',
				role: 'assistant' as const,
				type: 'reasoning' as const,
				content: '完整推理文本',
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'msg-00000000000000000000000000000009',
				role: 'assistant' as const,
				type: 'reasoning' as const,
				content: '完整推理文本',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		const reasoning = merged.filter((m) => m.type === 'reasoning');
		expect(reasoning).toHaveLength(1);
		expect(reasoning[0].id).toBe('msg-00000000000000000000000000000009');
	});

	it('keeps finalized live reasoning when the DB has no equivalent', () => {
		// The snap may arrive before the DB write; dropping it would lose
		// the block entirely.
		const db = [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' }];
		const existing = [
			{
				id: 'msg-00000000000000000000000000000009',
				role: 'assistant' as const,
				type: 'reasoning' as const,
				content: '新鲜推理',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toContain('msg-00000000000000000000000000000009');
	});

	it('keeps finalized live thought text missing from the DB', () => {
		const db = [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' }];
		const existing = [
			{
				id: 'msg-00000000000000000000000000000008',
				role: 'assistant' as const,
				content: '已定稿但未持久化',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toContain('msg-00000000000000000000000000000008');
	});

	it('keeps a finalized live-only assistant message before the DB row that follows it', () => {
		// The snap-finalized reasoning (msg-00000000000000000000000000000008) has no DB row yet, but the DB
		// already holds the user's later message (msg-00000000000000000000000000000002). The merge must not push
		// the reasoning AFTER that message (the old append-at-end behavior reordered
		// [user, thinking, user] to [user, user, thinking] for one frame).
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: 'second' },
		];
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000008', role: 'assistant' as const, content: '思考片段', streaming: false },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: 'second' },
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-00000000000000000000000000000008', 'msg-00000000000000000000000000000002']);
	});

	it('keeps a still-streaming live thought before a DB steer that follows it', () => {
		// Same reorder bug as the finalized case, but the leftover is still
		// streaming — the old unconditional streamingTail append pushed it
		// after the persisted steer for one frame.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: 'second' },
		];
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000008', role: 'assistant' as const, content: '思考中', streaming: true },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: 'second', steering: true },
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-00000000000000000000000000000008', 'msg-00000000000000000000000000000002']);
		expect(merged.find((m) => m.id === 'msg-00000000000000000000000000000008')!.streaming).toBe(true);
		expect(merged.find((m) => m.id === 'msg-00000000000000000000000000000002')!.steering).toBe(true);
	});

	it('preserves live steering on same-id user rows from the DB', () => {
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: '补充' },
		];
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi', received: true },
			{ id: 'msg-379388a05a9bb6d609e5b4f48e2062d8', role: 'assistant' as const, content: '想', streaming: true },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: '补充', steering: true },
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.find((m) => m.id === 'msg-00000000000000000000000000000002')!.steering).toBe(true);
	});

	it('reanchors pending steering after later DB-ordered agent messages', () => {
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: '补充' },
			{ id: 'msg-581584b22ed8f37635989297798f2af0', role: 'assistant' as const, content: '继续处理' },
		];
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-581584b22ed8f37635989297798f2af0', role: 'assistant' as const, content: '继续处理' },
			{ id: 'msg-00000000000000000000000000000002', role: 'user' as const, content: '补充', steering: true },
		];

		const merged = mergeLiveStreaming(db, existing);

		expect(merged.map((message) => message.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-581584b22ed8f37635989297798f2af0', 'msg-00000000000000000000000000000002']);
		expect(merged.at(-1)).toMatchObject({ id: 'msg-00000000000000000000000000000002', steering: true });
	});

	it('keeps the interrupted partial reasoning after a continue resync', () => {
		// After continue_session truncates the errored step's partial output
		// from the DB, the resync must NOT clear the already-streamed
		// "Thinking…" block. handleContinue preserves it by leaving it out of
		// partialIds; mergeLiveStreaming keeps it because the DB (post-truncate)
		// has no row with that id.
		const db = [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' }];
		const existing = [
			{
				id: 'msg-00000000000000000000000000000009',
				role: 'assistant' as const,
				type: 'reasoning' as const,
				content: '先想想一部分',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toContain('msg-00000000000000000000000000000009');
		expect(merged.find((m) => m.id === 'msg-00000000000000000000000000000009')!.content).toBe('先想想一部分');
	});

	it('keeps finalized step-* cards and standalone search while dropping transient web_search', () => {
		// Continue resync can race the retry's ToolCall/Observation and miss the
		// pending step row for one frame; dropping step-* cards made post-resume
		// tool calls vanish. web_search indicators are never persisted — drop.
		const db = [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' }];
		const existing = [
			{
				id: 'step-00000000000000000000000000000001',
				type: 'tool' as const,
				toolName: 'shell',
				content: 'Interrupted',
				streaming: false,
			},
			{
				id: 'tool-t-1-0-web_search',
				type: 'tool' as const,
				toolName: 'web_search',
				content: '已联网搜索',
				streaming: false,
			},
			{
				id: 'step-00000000000000000000000000000002',
				type: 'tool' as const,
				toolName: 'web_search_api',
				content: '{"results":[]}',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual([
			'msg-00000000000000000000000000000001',
			'step-00000000000000000000000000000001',
			'step-00000000000000000000000000000002',
		]);
		expect(merged.find((m) => m.id === 'step-00000000000000000000000000000001')).toMatchObject({
			type: 'tool' as const,
			content: 'Interrupted',
		});
	});

	it('drops finalized tool cards with malformed step identities', () => {
		const merged = mergeLiveStreaming(
			[],
			[
				{ id: 'step-short', type: 'tool' as const, streaming: false },
				{
					id: 'step-shorter-invalid',
					type: 'tool' as const,
					streaming: false,
				},
			],
		);
		expect(merged).toEqual([]);
	});

	it('keeps a finalized real user bubble until its DB row arrives', () => {
		const db = [{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' }];
		const existing = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{ id: 'msg-7cfb761c263b30e8eef2e0fafe90c089', role: 'user' as const, content: '补充说明', streaming: false },
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'msg-7cfb761c263b30e8eef2e0fafe90c089']);
	});

	it('keeps an additional presentation of a transcript entity distinct during resume merge', () => {
		const messageId = 'msg-00000000000000000000000000000072';
		const db = [{ id: messageId, role: 'user' as const, content: 'ToolRun result' }];
		const existing = [
			{ id: messageId, role: 'user' as const, content: 'ToolRun result' },
			{
				id: messageId,
				presentationKey: `tool-run-result:${messageId}:toolrun-00000000000000000000000000000072`,
				type: 'tool' as const,
				toolName: 'tool_runs.inspect',
				content: '{"operation":"tool_runs_result_injected"}',
			},
		];

		const merged = mergeLiveStreaming(db, existing);

		expect(merged).toHaveLength(2);
		expect(merged.map((message) => message.id)).toEqual([messageId, messageId]);
		expect(merged.map((message) => message.presentationKey ?? message.id)).toEqual([
			messageId,
			existing[1].presentationKey,
		]);
	});

	it('prefers a live awaiting ask card over the DB ask card of the same id', () => {
		// The DB build may lack quick-reply options/awaiting (the pause status
		// can land after the observation); the awaiting live card wins for the
		// same step id so the user can answer.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'step-00000000000000000000000000000007',
				role: 'assistant' as const,
				type: 'ask' as const,
				content: '继续吗？',
				options: [],
				awaiting: false,
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'step-00000000000000000000000000000007',
				type: 'ask' as const,
				toolName: 'ask',
				content: '继续吗？',
				options: ['A', 'B'],
				awaiting: true,
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		const asks = merged.filter((m) => m.type === 'ask');
		expect(asks).toHaveLength(1);
		expect(asks[0]).toMatchObject({ id: 'step-00000000000000000000000000000007', options: ['A', 'B'], awaiting: true });
	});

	it('prefers EVERY awaiting live ask card in a batched step', () => {
		// Two asks in one batch: both live cards are awaiting in the
		// observation→pause race window; both must keep their options, not
		// just the first.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'step-00000000000000000000000000000001',
				role: 'assistant' as const,
				type: 'ask' as const,
				content: 'Q1？',
				options: [],
				awaiting: false,
				streaming: false,
			},
			{
				id: 'step-00000000000000000000000000000002',
				role: 'assistant' as const,
				type: 'ask' as const,
				content: 'Q2？',
				options: [],
				awaiting: false,
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'step-00000000000000000000000000000001',
				type: 'ask' as const,
				toolName: 'ask',
				content: 'Q1？',
				options: ['A'],
				awaiting: true,
				streaming: false,
			},
			{
				id: 'step-00000000000000000000000000000002',
				type: 'ask' as const,
				toolName: 'ask',
				content: 'Q2？',
				options: ['B'],
				awaiting: true,
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		const asks = merged.filter((m) => m.type === 'ask');
		expect(asks).toHaveLength(2);
		expect(asks[0]).toMatchObject({ id: 'step-00000000000000000000000000000001', options: ['A'], awaiting: true });
		expect(asks[1]).toMatchObject({ id: 'step-00000000000000000000000000000002', options: ['B'], awaiting: true });
	});

	it('keeps the live streaming tool card over its empty DB badge mid-tool', () => {
		// The step row exists from tool start with a NULL observation, so the
		// DB badge is EMPTY while the live card is still streaming. Switching
		// sessions mid-tool must keep the live card, not freeze an empty
		// badge (the old dropToolSteps behavior, now id-based).
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'step-00000000000000000000000000000009',
				type: 'tool' as const,
				toolName: 'shell',
				stepNumber: 2,
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'step-00000000000000000000000000000009',
				type: 'tool' as const,
				toolName: 'shell',
				stepNumber: 2,
				content: '',
				streaming: true,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged.map((m) => m.id)).toEqual(['msg-00000000000000000000000000000001', 'step-00000000000000000000000000000009']);
		expect(merged.find((m) => m.id === 'step-00000000000000000000000000000009')).toMatchObject({ streaming: true });
	});

	it('lets a FINALIZED live tool card yield to the DB badge', () => {
		// Once the observation lands (live card finalized), the DB copy wins
		// as before — the streaming preference only applies mid-tool.
		const db = [
			{ id: 'msg-00000000000000000000000000000001', role: 'user' as const, content: 'hi' },
			{
				id: 'step-00000000000000000000000000000009',
				type: 'tool' as const,
				toolName: 'shell',
				stepNumber: 2,
				content: 'done',
				streaming: false,
			},
		];
		const existing = [
			{
				id: 'step-00000000000000000000000000000009',
				type: 'tool' as const,
				toolName: 'shell',
				stepNumber: 2,
				content: 'done',
				streaming: false,
			},
		];
		const merged = mergeLiveStreaming(db, existing);
		expect(merged).toEqual(db);
	});
});
