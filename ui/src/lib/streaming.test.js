import { describe, it, expect } from 'vitest';
import { accumulateStreamChunk, applyThoughtSnap, thoughtSegmentIds, stepId, toolId, finalizeStreamBlocks, newToolMessage } from './streaming.js';

const STEP_ID = 'thought-t-1-0';
const REASONING_ID = 'reasoning-t-1-0';
const BASE = {
	stepId: STEP_ID,
	stepIdPrefix: 'thought',
	msgType: undefined,
	stepNumber: 1,
	time: '10:00',
};

const chunk = (messages, delta, opts = {}) =>
	accumulateStreamChunk(messages, { ...BASE, delta, ...opts });

const snap = (messages, thought, opts = {}) =>
	applyThoughtSnap(messages, {
		stepId: STEP_ID,
		reasoningId: REASONING_ID,
		thought,
		stepNumber: 1,
		time: '10:00',
		...opts,
	});

const last = (messages) => messages[messages.length - 1];

describe('accumulateStreamChunk (thought)', () => {
	it('creates a streaming message for the first chunk without a boundary', () => {
		const out = chunk([], '好的');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的',
			streaming: true,
			segmented: false,
		});
	});

	it('finalizes immediately when the first chunk completes a sentence', () => {
		const out = chunk([], '好的。');
		expect(out[0]).toMatchObject({ content: '好的。', streaming: false, segmented: true });
	});

	it('appends incremental deltas to the open segment', () => {
		let m = chunk([], '好的');
		m = chunk(m, '，我先');
		m = chunk(m, '查一下');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({ content: '好的，我先查一下', streaming: true });
	});

	it('finalizes at a sentence boundary mid-stream', () => {
		let m = chunk([], '好的');
		m = chunk(m, '，我先查一下。');
		expect(last(m)).toMatchObject({ content: '好的，我先查一下。', streaming: false, segmented: true });
	});

	it('opens a new segment when the next chunk arrives after a boundary', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度');
		expect(m).toHaveLength(2);
		expect(m[1]).toMatchObject({
			id: `${STEP_ID}-1`,
			content: '今天20度',
			streaming: true,
			segmented: false,
		});
	});

	it('finalizes the opening chunk of a new segment when it ends a sentence', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度。');
		expect(m).toHaveLength(2);
		expect(m[1]).toMatchObject({ content: '今天20度。', streaming: false, segmented: true });
	});

	it('keeps segment numbering unique across multiple splits', () => {
		let m = chunk([], '一。');
		m = chunk(m, '二。');
		m = chunk(m, '三');
		expect(m.map((x) => x.id)).toEqual([STEP_ID, `${STEP_ID}-1`, `${STEP_ID}-2`]);
	});

	it('does not split inside an unclosed code fence', () => {
		let m = chunk([], '```js');
		m = chunk(m, 'console.log("a。")');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({ content: '```jsconsole.log("a。")', streaming: true });
	});

	it('finalizes at the boundary once the code fence is closed', () => {
		let m = chunk([], '```js\nx。\n```');
		expect(last(m)).toMatchObject({ streaming: true });
		m = chunk(m, '之后的话。');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({
			content: '```js\nx。\n```之后的话。',
			streaming: false,
			segmented: true,
		});
	});

	it('drops straggler chunks after a snap finalization', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度');
		m = snap(m, '好的。今天20度');
		const before = m;
		const out = chunk(m, '残留');
		expect(out).toBe(before);
	});

	it('resumes streaming in place when a cumulative echo follows a boundary', () => {
		let m = chunk([], '好的。');
		const out = chunk(m, '好的。今天20度。');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			content: '好的。今天20度。',
			streaming: true,
			segmented: false,
		});
	});

	it('replaces content for cumulative providers (no splitting)', () => {
		let m = chunk([], '好的');
		m = chunk(m, '好的，我先查一下');
		m = chunk(m, '好的，我先查一下。今天20度。');
		expect(m).toHaveLength(1);
		expect(m[0].content).toBe('好的，我先查一下。今天20度。');
		expect(m[0].streaming).toBe(true);
	});

	it('collapses segments when a cumulative echo follows a sentence split', () => {
		// After a boundary the stream holds `A。` + `B`. A cumulative
		// provider then echoes the FULL text `A。B。C` — comparing only
		// against the last segment (`B`) would concatenate garbage
		// (`BA。B。C`). All segments must collapse into one message.
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度');
		const out = chunk(m, '好的。今天20度。适合出门');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的。今天20度。适合出门',
			streaming: true,
			segmented: false,
		});
	});

	it('collapses segments when a cumulative echo resumes a finalized split', () => {
		// The split segment was finalized (streaming: false, segmented:
		// true); the echo reopens it in place with the full text.
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度。');
		const out = chunk(m, '好的。今天20度。适合出门。');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的。今天20度。适合出门。',
			streaming: true,
			segmented: false,
		});
	});

	it('keeps reasoning in front when collapsing segments over a cumulative echo', () => {
		// Same index-mapping class as the original applyThoughtSnap bug: the
		// collapse splices the merged message at an index computed against
		// `messages`, applied to `rest` with the segments removed. A
		// reasoning block sitting before the segments must stay above the
		// merged answer, not sink below it.
		let m = chunk([], '好的。');
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '先想想',
			streaming: true,
		};
		m = [reasoning, ...m];
		m = chunk(m, '今天20度');
		const out = chunk(m, '好的。今天20度。适合出门');
		expect(out.map((x) => x.id)).toEqual([REASONING_ID, STEP_ID]);
		expect(out[1]).toMatchObject({
			content: '好的。今天20度。适合出门',
			streaming: true,
			segmented: false,
		});
	});
});

describe('accumulateStreamChunk (reasoning)', () => {
	it('never splits reasoning into segments', () => {
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		let m = accumulateStreamChunk([], { ...base, delta: '先想想。' });
		m = accumulateStreamChunk(m, { ...base, delta: '再想想。' });
		expect(m).toHaveLength(1);
		expect(m[0]).toMatchObject({ id: REASONING_ID, content: '先想想。再想想。', streaming: true });
	});

	it('drops chunks after the reasoning block was finalized', () => {
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		let m = accumulateStreamChunk([], { ...base, delta: '想想' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, { ...base, delta: '多余' });
		expect(out).toBe(m);
	});

	it('accepts the authoritative reconciliation delta after finalization', () => {
		// The backend emits the COMPLETE reasoning text as a final chunk
		// after the stream is finalized (batcher-flush reconciliation).
		// It must replace the content, not be dropped — otherwise dropped
		// trailing characters are lost forever.
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		let m = accumulateStreamChunk([], { ...base, delta: '思考了一部分' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, { ...base, delta: '思考了一部分，还有更多' });
		expect(out).toHaveLength(1);
		expect(out[0].content).toBe('思考了一部分，还有更多');
	});

	it('rejects a stale incremental delta after finalization', () => {
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		let m = accumulateStreamChunk([], { ...base, delta: '想想' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, { ...base, delta: '不匹配的增量' });
		expect(out).toBe(m);
	});

	it('inserts a late reasoning block in front of the thought, not below it', () => {
		// Interleaved providers may emit text first and reasoning after.
		// The reasoning must sit ABOVE the answer from the first chunk —
		// appending it at the end would show Thinking... below the content
		// until the snap reorders it.
		let m = chunk([], '回答文字。');
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		m = accumulateStreamChunk(m, { ...base, delta: '迟到的推理' });
		expect(m.map((x) => x.id)).toEqual([REASONING_ID, STEP_ID]);
		expect(m[0]).toMatchObject({ id: REASONING_ID, content: '迟到的推理', streaming: true });
	});

	it('appends a reasoning block when no thought segment exists yet', () => {
		const base = { ...BASE, stepIdPrefix: 'reasoning', stepId: REASONING_ID, msgType: 'reasoning' };
		const m = accumulateStreamChunk([], { ...base, delta: '先推理' });
		expect(m).toHaveLength(1);
		expect(m[0].id).toBe(REASONING_ID);
	});
});

describe('applyThoughtSnap', () => {
	it('creates the message when no segments exist yet', () => {
		const out = snap([], '完整的回答。');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({ id: STEP_ID, content: '完整的回答。', streaming: false });
	});

	it('collapses all segments into a single message on snap', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度');
		const out = snap(m, '好的。今天20度');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的。今天20度',
			streaming: false,
		});
	});

	it('uses the authoritative snap text when deltas were dropped', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度'); // "，适合出门" was dropped
		const out = snap(m, '好的。今天20度，适合出门');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的。今天20度，适合出门',
			streaming: false,
		});
	});

	it('replaces all segments when the stream diverged (retry/fallback)', () => {
		let m = chunk([], '坏掉的尝试');
		m = chunk(m, '重试的完整回答。');
		const out = snap(m, '重试的完整回答。');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({ id: STEP_ID, content: '重试的完整回答。', streaming: false });
	});

	it('drops straggler chunks after a snap finalization', () => {
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度');
		m = snap(m, '好的。今天20度');
		const before = m;
		const out = chunk(m, '残留');
		expect(out).toBe(before);
	});

	it('finalizes a streaming reasoning block of the same step', () => {
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '思考中',
			streaming: true,
		};
		const out = snap([reasoning], '回答');
		expect(out.find((x) => x.id === REASONING_ID)).toMatchObject({ streaming: false });
		expect(out.find((x) => x.id === STEP_ID)).toMatchObject({ content: '回答' });
	});

	it('moves a trailing reasoning block in front of the merged thought', () => {
		// Interleaved providers may stream reasoning AFTER the thought
		// segments (text first). The snap must not leave the final order
		// as [answer, Thinking...].
		let m = chunk([], '回答文字。');
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '迟到的推理',
			streaming: true,
		};
		m = [...m, reasoning];
		const out = snap(m, '回答文字。');
		expect(out.map((x) => x.id)).toEqual([REASONING_ID, STEP_ID]);
		expect(out[0]).toMatchObject({ content: '迟到的推理', streaming: false });
		expect(out[1]).toMatchObject({ content: '回答文字。', streaming: false });
	});

	it('keeps a leading reasoning block in front of the merged thought', () => {
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '先推理',
			streaming: true,
		};
		let m = [reasoning];
		m = chunk(m, '回答文字。');
		const out = snap(m, '回答文字。');
		expect(out.map((x) => x.id)).toEqual([REASONING_ID, STEP_ID]);
	});

	it('keeps the user question before the reasoning (does not jump above it)', () => {
		// Reported bug: with [user, reasoning, thought-seg], the off-by-one
		// insertion pushed thinking ABOVE the user question.
		const user = { id: 'user-1', role: 'user', content: '问题' };
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '思考中',
			streaming: true,
		};
		let m = [user, reasoning];
		m = chunk(m, '回答。');
		const out = snap(m, '回答。');
		expect(out.map((x) => x.id)).toEqual(['user-1', REASONING_ID, STEP_ID]);
	});

	it('keeps a tool card in order when collapsing a later step', () => {
		// A prior tool card must not sink below the merged thought/reasoning
		// when a subsequent step's segments are collapsed.
		const user = { id: 'user-1', role: 'user', content: '问题' };
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '先查一下',
			streaming: true,
		};
		const tool = {
			id: 'tool-t-1-0-1',
			role: 'assistant',
			type: 'tool',
			content: '观察结果',
			streaming: false,
			stepNumber: 1,
		};
		let m = [user, reasoning];
		m = chunk(m, '我先查一下。');
		m = [...m, tool];
		const out = snap(m, '我先查一下。');
		expect(out.map((x) => x.id)).toEqual(['user-1', REASONING_ID, STEP_ID, tool.id]);
	});
});

describe('thoughtSegmentIds', () => {
	it('matches the base id and its numbered segments only', () => {
		const m = [
			{ id: 'thought-t-1-0' },
			{ id: 'thought-t-1-0-1' },
			{ id: 'thought-t-2-0' },
			{ id: 'reasoning-t-1-0' },
		];
		expect(thoughtSegmentIds(m, 'thought-t-1-0')).toEqual(['thought-t-1-0', 'thought-t-1-0-1']);
	});
});

describe('stepId / toolId factories', () => {
	it('builds prefixed ids with a default run of 0', () => {
		expect(stepId('thought', 't', 3, 7)).toBe('thought-t-3-7');
		expect(stepId('thought', 't', 3, undefined)).toBe('thought-t-3-0');
		expect(stepId('reasoning', 't', 3, null)).toBe('reasoning-t-3-0');
		expect(stepId('tool', 't', 3, 0)).toBe('tool-t-3-0');
	});

	it('builds tool ids by appending the call id or name', () => {
		expect(toolId('t', 3, 7, 'call-1')).toBe('tool-t-3-7-call-1');
		expect(toolId('t', 3, undefined, 'read_file')).toBe('tool-t-3-0-read_file');
	});

	it('produces the exact id shape the old template literals produced', () => {
		// Regression guard: +page.svelte's streaming handlers must not drift.
		const tid = 't1';
		const step = 5;
		const run = 2;
		expect(stepId('thought', tid, step, run)).toBe(`thought-${tid}-${step}-${run}`);
		expect(stepId('reasoning', tid, step, run)).toBe(`reasoning-${tid}-${step}-${run}`);
		expect(toolId(tid, step, run, 'call-9')).toBe(`tool-${tid}-${step}-${run}-call-9`);
		expect(toolId(tid, step, run, 'file')).toBe(`tool-${tid}-${step}-${run}-file`);
	});
});

describe('finalizeStreamBlocks', () => {
	it('finalizes reasoning, thought, and thought segments; leaves others alone', () => {
		const m = [
			{ id: 'reasoning-t-1-0', streaming: true },
			{ id: 'thought-t-1-0', streaming: true, segmented: true },
			{ id: 'thought-t-1-0-1', streaming: true, segmented: true },
			{ id: 'thought-t-2-0', streaming: true },
			{ id: 'm1', streaming: true },
		];
		const out = finalizeStreamBlocks(m, 'reasoning-t-1-0', 'thought-t-1-0');
		expect(out.find((x) => x.id === 'reasoning-t-1-0')).toMatchObject({ streaming: false, segmented: false });
		expect(out.find((x) => x.id === 'thought-t-1-0')).toMatchObject({ streaming: false, segmented: false });
		expect(out.find((x) => x.id === 'thought-t-1-0-1')).toMatchObject({ streaming: false, segmented: false });
		expect(out.find((x) => x.id === 'thought-t-2-0')).toMatchObject({ streaming: true });
		expect(out.find((x) => x.id === 'm1')).toMatchObject({ streaming: true });
	});

	it('is a no-op when reasoning is missing but the thought exists', () => {
		const m = [{ id: 'thought-t-1-0', streaming: true }];
		const out = finalizeStreamBlocks(m, 'reasoning-t-1-0', 'thought-t-1-0');
		expect(out.find((x) => x.id === 'thought-t-1-0')).toMatchObject({ streaming: false });
	});
});

describe('newToolMessage', () => {
	it('builds a plain tool message', () => {
		const msg = newToolMessage({ id: 'tool-t-1-0-call1', stepNumber: 1, toolName: 'file', time: '10:00' });
		expect(msg).toEqual({
			id: 'tool-t-1-0-call1',
			role: 'assistant',
			content: '',
			toolName: 'file',
			type: 'tool',
			voice: false,
			stepNumber: 1,
			time: '10:00',
			streaming: false,
		});
	});

	it('marks ask messages as question cards with options and awaiting', () => {
		const msg = newToolMessage({
			id: 'tool-t-1-0-call1',
			stepNumber: 1,
			toolName: 'ask',
			content: '继续吗？',
			askOptions: ['A', 'B'],
		});
		expect(msg.type).toBe('ask');
		expect(msg.options).toEqual(['A', 'B']);
		expect(msg.awaiting).toBe(true);
	});

	it('omits time when falsy so observation fills preserve the placeholder timestamp', () => {
		const msg = newToolMessage({ id: 'x', stepNumber: 1, toolName: 'file', content: 'ok' });
		expect('time' in msg).toBe(false);
	});
});
