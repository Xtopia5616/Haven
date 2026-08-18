import { describe, it, expect } from 'vitest';
import { accumulateStreamChunk, applyThoughtSnap, webSearchId, finalizeStreamBlocks, newToolMessage } from './streaming.ts';

const STEP_ID = 'msg-thought-1';
const REASONING_ID = 'msg-reasoning-1';
const BASE = {
	messageId: STEP_ID,
	msgType: undefined,
	stepNumber: 1,
	runId: 0,
	time: '10:00',
};

const chunk = (messages, delta, opts = {}) =>
	accumulateStreamChunk(messages, { ...BASE, delta, ...opts });

const snap = (messages, thought, opts = {}) =>
	applyThoughtSnap(messages, {
		messageId: STEP_ID,
		reasoningId: REASONING_ID,
		thought,
		stepNumber: 1,
		runId: 0,
		time: '10:00',
		...opts,
	});

const last = (messages) => messages[messages.length - 1];

describe('accumulateStreamChunk (thought)', () => {
	it('creates a streaming message for the first chunk', () => {
		const out = chunk([], '好的');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({
			id: STEP_ID,
			content: '好的',
			streaming: true,
		});
	});

	it('appends incremental deltas to the streaming message', () => {
		let m = chunk([], '好的');
		m = chunk(m, '，我先');
		m = chunk(m, '查一下');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({ content: '好的，我先查一下', streaming: true });
	});

	it('never splits into separate bubbles at sentence boundaries', () => {
		// Regression: sentence-completing chunks used to finalize the message
		// and open a new segment, so the answer showed as several bubbles
		// while streaming and only merged after the snap.
		let m = chunk([], '好的。');
		m = chunk(m, '今天20度。');
		m = chunk(m, '适合出门。');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({
			id: STEP_ID,
			content: '好的。今天20度。适合出门。',
			streaming: true,
		});
	});

	it('does not split inside a code fence', () => {
		let m = chunk([], '```js');
		m = chunk(m, 'console.log("a。")');
		m = chunk(m, '```');
		expect(m).toHaveLength(1);
		expect(last(m)).toMatchObject({ content: '```jsconsole.log("a。")```', streaming: true });
	});

	it('replaces content for cumulative providers (no splitting)', () => {
		let m = chunk([], '好的');
		m = chunk(m, '好的，我先查一下');
		m = chunk(m, '好的，我先查一下。今天20度。');
		expect(m).toHaveLength(1);
		expect(m[0].content).toBe('好的，我先查一下。今天20度。');
		expect(m[0].streaming).toBe(true);
	});

	it('drops straggler chunks after a snap finalization', () => {
		let m = chunk([], '好的');
		m = chunk(m, '今天20度');
		m = snap(m, '好的今天20度');
		const before = m;
		const out = chunk(m, '残留');
		expect(out).toBe(before);
	});

	it('accepts a full-text reconcile after finalization', () => {
		// A dropped middle batch leaves the accumulated content a
		// prefix-MISMATCHED partial of the authoritative text. The final
		// full-text delta must still replace it (length-based).
		let m = chunk([], '开头的回答');
		m = chunk(m, '结尾');
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = chunk(m, '开头的回答，中间被丢掉的内容，结尾');
		expect(out).toHaveLength(1);
		expect(out[0].content).toBe('开头的回答，中间被丢掉的内容，结尾');
	});

	it('rejects a stale incremental delta after finalization', () => {
		let m = chunk([], '回答');
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = chunk(m, '多余');
		expect(out).toBe(m);
	});
});

describe('accumulateStreamChunk (reasoning)', () => {
	const base = { ...BASE, messageId: REASONING_ID, msgType: 'reasoning' };

	it('never splits reasoning into segments', () => {
		let m = accumulateStreamChunk([], { ...base, delta: '先想想。' });
		m = accumulateStreamChunk(m, { ...base, delta: '再想想。' });
		expect(m).toHaveLength(1);
		expect(m[0]).toMatchObject({ id: REASONING_ID, content: '先想想。再想想。', streaming: true });
	});

	it('drops chunks after the reasoning block was finalized', () => {
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
		let m = accumulateStreamChunk([], { ...base, delta: '思考了一部分' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, { ...base, delta: '思考了一部分，还有更多' });
		expect(out).toHaveLength(1);
		expect(out[0].content).toBe('思考了一部分，还有更多');
	});

	it('accepts a full-text reconcile when intermediate chunks were dropped', () => {
		// A dropped middle batch leaves the accumulated content a
		// prefix-MISMATCHED partial of the authoritative text. The final
		// full-text reconcile must still replace it (length-based), or the
		// reasoning block is permanently truncated.
		let m = accumulateStreamChunk([], { ...base, delta: '开头的思考' });
		m = accumulateStreamChunk(m, { ...base, delta: '结尾' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, {
			...base,
			delta: '开头的思考，中间被丢掉的内容，结尾',
		});
		expect(out).toHaveLength(1);
		expect(out[0].content).toBe('开头的思考，中间被丢掉的内容，结尾');
	});

	it('rejects a stale incremental delta after finalization', () => {
		let m = accumulateStreamChunk([], { ...base, delta: '想想' });
		m = m.map((x) => ({ ...x, streaming: false }));
		const out = accumulateStreamChunk(m, { ...base, delta: '多余' });
		expect(out).toBe(m);
	});

	it('inserts a late reasoning block in front of the thought, not below it', () => {
		// Interleaved providers may emit text first and reasoning after.
		// The reasoning must sit ABOVE the answer from the first chunk —
		// appending it at the end would show Thinking... below the content
		// until the snap reorders it. The same-step thought is found by
		// (stepNumber, runId), since message ids carry no step information.
		let m = chunk([], '回答文字。');
		m = accumulateStreamChunk(m, { ...base, delta: '迟到的推理' });
		expect(m.map((x) => x.id)).toEqual([REASONING_ID, STEP_ID]);
		expect(m[0]).toMatchObject({ id: REASONING_ID, content: '迟到的推理', streaming: true });
	});

	it('does not climb above another step with the same number but a different run', () => {
		// A resumed session reuses step numbers; the reasoning of run 2 must
		// anchor on run 2's thought, not run 1's.
		const run1Thought = {
			id: 'msg-run1',
			role: 'assistant',
			type: undefined,
			content: '上一次的回答',
			stepNumber: 1,
			runId: 1,
			streaming: false,
		};
		let m = [run1Thought];
		m = chunk(m, '这次的回答。');
		m = accumulateStreamChunk(m, { ...base, delta: '这次的推理' });
		expect(m.map((x) => x.id)).toEqual([run1Thought.id, REASONING_ID, STEP_ID]);
	});

	it('appends a reasoning block when no thought message exists yet', () => {
		const m = accumulateStreamChunk([], { ...base, delta: '先推理' });
		expect(m).toHaveLength(1);
		expect(m[0].id).toBe(REASONING_ID);
	});
});

describe('applyThoughtSnap', () => {
	it('creates the message when no streamed thought exists yet', () => {
		const out = snap([], '完整的回答。');
		expect(out).toHaveLength(1);
		expect(out[0]).toMatchObject({ id: STEP_ID, content: '完整的回答。', streaming: false });
	});

	it('finalizes and reconciles the streamed thought on snap', () => {
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

	it('replaces the streamed text when the stream diverged (retry/fallback)', () => {
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
		// (text first). The snap must not leave the final order as
		// [answer, Thinking...].
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
		// Reported bug: with [user, reasoning, thought], the off-by-one
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
		// when a subsequent step's message is collapsed.
		const user = { id: 'user-1', role: 'user', content: '问题' };
		const reasoning = {
			id: REASONING_ID,
			role: 'assistant',
			content: '先查一下',
			streaming: true,
		};
		const tool = {
			id: 'step-tool-1',
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

	it('is a no-op replace when the DB message (same id) is already in the list', () => {
		// The turn finished and the list was rebuilt from the DB, then a
		// replayed/late `agent:thought` snap arrives. The snap carries the
		// SAME id the DB copy has, so the reconcile is a plain replace with
		// identical content — no duplicate is appended.
		const dbCopy = {
			id: STEP_ID,
			role: 'assistant',
			content: '完整的回答。',
			streaming: false,
		};
		const m = [{ id: 'user-1', role: 'user', content: '问题' }, dbCopy];
		const out = snap(m, '完整的回答。');
		expect(out).toEqual(m);
		expect(out.filter((x) => x.content === '完整的回答。')).toHaveLength(1);
	});

	it('reconciles replayed chunks onto the DB copy without duplicating it', () => {
		// Chunks replayed on a fresh context after a remount accumulate onto
		// the DB copy (same id); the snap then settles it to the identical
		// authoritative text — one bubble, streaming flag cleared.
		const dbCopy = {
			id: STEP_ID,
			role: 'assistant',
			content: '完整的回答。',
			streaming: false,
		};
		let m = [dbCopy];
		m = chunk(m, '完整的回');
		m = chunk(m, '答。');
		const out = snap(m, '完整的回答。');
		expect(out).toEqual([dbCopy]);
		expect(out).toHaveLength(1);
	});

	it('keeps appending when the list has no equivalent content', () => {
		// A fresh page that missed the stream still gets the full text.
		const m = [{ id: 'user-1', role: 'user', content: '问题' }];
		const out = snap(m, '完整的回答。');
		expect(out.map((x) => x.id)).toEqual(['user-1', STEP_ID]);
		expect(out[1]).toMatchObject({ content: '完整的回答。', streaming: false });
	});
});

describe('webSearchId', () => {
	it('builds the ephemeral web-search card id with a default run of 0', () => {
		expect(webSearchId('t', 3, 7)).toBe('tool-t-3-7-web_search');
		expect(webSearchId('t', 3, undefined)).toBe('tool-t-3-0-web_search');
	});
});

describe('finalizeStreamBlocks', () => {
	it('finalizes reasoning and thought blocks; leaves others alone', () => {
		const m = [
			{ id: 'msg-reasoning-1', streaming: true },
			{ id: 'msg-thought-1', streaming: true },
			{ id: 'msg-thought-2', streaming: true },
			{ id: 'm1', streaming: true },
		];
		const out = finalizeStreamBlocks(m, 'msg-reasoning-1', 'msg-thought-1');
		expect(out.find((x) => x.id === 'msg-reasoning-1')).toMatchObject({ streaming: false });
		expect(out.find((x) => x.id === 'msg-thought-1')).toMatchObject({ streaming: false });
		expect(out.find((x) => x.id === 'msg-thought-2')).toMatchObject({ streaming: true });
		expect(out.find((x) => x.id === 'm1')).toMatchObject({ streaming: true });
	});

	it('is a no-op when reasoning is missing but the thought exists', () => {
		const m = [{ id: 'msg-thought-1', streaming: true }];
		const out = finalizeStreamBlocks(m, 'msg-reasoning-1', 'msg-thought-1');
		expect(out.find((x) => x.id === 'msg-thought-1')).toMatchObject({ streaming: false });
	});

	it('is a no-op when both ids are unknown', () => {
		const m = [{ id: 'm1', streaming: true }];
		const out = finalizeStreamBlocks(m, undefined, undefined);
		expect(out).toEqual(m);
	});
});

describe('newToolMessage', () => {
	it('builds a plain tool message', () => {
		const msg = newToolMessage({ id: 'step-1', stepNumber: 1, toolName: 'file', time: '10:00' });
		expect(msg).toEqual({
			id: 'step-1',
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
			id: 'step-1',
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
