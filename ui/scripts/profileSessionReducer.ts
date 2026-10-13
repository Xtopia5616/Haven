import { performance } from 'node:perf_hooks';
import { writable } from 'svelte/store';

import {
	initialSessionState,
	SessionReducer,
	type SessionAction,
	type SessionMessage,
	type SessionReducerState,
} from '../src/lib/sessionReducer.ts';
import { createEqualityGatedSessionSelectorStore } from '../src/lib/sessionReducer/selectorStore.ts';

const SAMPLE_COUNT = 513;
const REPEAT_COUNT = 3;
const WARMUP_COUNT = 16;
const SESSION_ID = 'ses-performance-profile';
const MESSAGE_ID = 'step-performance-profile';
const EMPTY_MESSAGES: SessionMessage[] = [];

function percentile(samples: readonly number[], numerator: number): number {
	const ordered = [...samples].sort((left, right) => left - right);
	const rank = Math.ceil((ordered.length * numerator) / 100);
	return ordered[Math.max(0, rank - 1)];
}

function action(seq: number): SessionAction {
	return {
		type: 'agent/chunks',
		chunks: [
			{
				kind: 'thought',
				msgType: 'thought',
				payload: {
					sessionId: SESSION_ID,
					messageId: MESSAGE_ID,
					stepNumber: 1,
					runId: 1,
					seq,
					delta: 'x',
				},
			},
		],
	};
}

function profile(messageCount: number, selectorSubscribers: number) {
	const messages = Array.from({ length: messageCount }, (_, index): SessionMessage => ({
		id: index === messageCount - 1 ? MESSAGE_ID : `msg-profile-${index}`,
		role: index === messageCount - 1 ? 'assistant' : 'user',
		content: 'profile fixture',
		streaming: index === messageCount - 1,
		stepNumber: index === messageCount - 1 ? 1 : undefined,
		runId: index === messageCount - 1 ? 1 : undefined,
	}));
	const initialState: SessionReducerState = {
		...initialSessionState,
		activeSessionId: SESSION_ID,
		messages: { [SESSION_ID]: messages },
	};
	const source = writable(initialState);
	const reducer = new SessionReducer(initialState, source);
	const selector = createEqualityGatedSessionSelectorStore(
		source,
		(state) => state.messages[SESSION_ID] ?? EMPTY_MESSAGES,
	);
	let sourceNotifications = 0;
	let reducerNotifications = 0;
	let selectorNotifications = 0;
	const unsubscribeSource = source.subscribe(() => sourceNotifications++);
	const unsubscribeReducer = reducer.subscribe(() => reducerNotifications++);
	const unsubscribeSelectors = Array.from({ length: selectorSubscribers }, () =>
		selector.subscribe(() => selectorNotifications++),
	);
	sourceNotifications = 0;
	reducerNotifications = 0;
	selectorNotifications = 0;

	let seq = 0;
	for (let index = 0; index < WARMUP_COUNT; index++) reducer.dispatch(action(seq++));
	sourceNotifications = 0;
	reducerNotifications = 0;
	selectorNotifications = 0;
	const samplesMs = [];
	const wallStarted = performance.now();
	for (let index = 0; index < SAMPLE_COUNT; index++) {
		const started = performance.now();
		reducer.dispatch(action(seq++));
		samplesMs.push(performance.now() - started);
	}
	const wallElapsedMs = performance.now() - wallStarted;
	const notifications = {
		source: sourceNotifications,
		reducer: reducerNotifications,
		selectors: selectorNotifications,
		total: sourceNotifications + reducerNotifications + selectorNotifications,
	};

	for (const unsubscribe of unsubscribeSelectors) unsubscribe();
	unsubscribeReducer();
	unsubscribeSource();
	return {
		messageCount,
		selectorSubscribers,
		samplesMs,
		wallElapsedMs,
		notificationsPerSample: notifications.total / SAMPLE_COUNT,
		selectorNotificationsPerSample: notifications.selectors / SAMPLE_COUNT,
	};
}

for (const messageCount of [1_000, 10_000]) {
	for (const selectorSubscribers of [0, 16, 64]) {
		const runs = Array.from({ length: REPEAT_COUNT }, () =>
			profile(messageCount, selectorSubscribers),
		);
		const samplesMs = runs.flatMap((run) => run.samplesMs);
		const wallElapsedMs = runs.reduce((total, run) => total + run.wallElapsedMs, 0);
		const result = {
			messageCount,
			selectorSubscribers,
			p50Us: percentile(samplesMs, 50) * 1000,
			p95Us: percentile(samplesMs, 95) * 1000,
			throughput: (samplesMs.length * 1000) / wallElapsedMs,
			notificationsPerSample:
				runs.reduce((total, run) => total + run.notificationsPerSample, 0) / runs.length,
			selectorNotificationsPerSample:
				runs.reduce((total, run) => total + run.selectorNotificationsPerSample, 0) /
				runs.length,
		};
		process.stdout.write(
			`profile ui_reducer scenario=agent_chunk_dispatch messages=${result.messageCount} active_selector_subscribers=${result.selectorSubscribers} samples=${samplesMs.length} repeats=${REPEAT_COUNT} warmup_per_repeat=${WARMUP_COUNT} notifications_per_dispatch=${result.notificationsPerSample.toFixed(1)} selector_notifications_per_dispatch=${result.selectorNotificationsPerSample.toFixed(1)} p50_us=${result.p50Us.toFixed(2)} p95_us=${result.p95Us.toFixed(2)} throughput_per_s=${result.throughput.toFixed(1)}\n`,
		);
	}
}
