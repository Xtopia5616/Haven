import { beforeEach, describe, expect, it, vi } from 'vitest';

const listen = vi.hoisted(() => vi.fn());

vi.mock('./tauri.ts', () => ({ listen }));

import {
	createChatEventController,
	type ChatEventControllerDependencies,
	type ChatEventRegistration,
	type ChatEventRegistrationPort,
} from './chatEventController.ts';

function makeDependencies(): ChatEventControllerDependencies {
	return {
		getActiveSessionId: () => null,
		isFreshSessionIntent: () => false,
		adoptDraftMessages: () => false,
		dispatchSession: vi.fn(),
		getErroredSessionId: () => null,
		clearAskAwaiting: vi.fn(),
		evictTerminalSessionMemory: vi.fn(),
		clearStepBlockIds: vi.fn(),
		flushChunksNow: vi.fn(),
		updateSessionTitle: vi.fn(),
		scheduleLoadSessions: vi.fn(),
		chunkHandler: () => () => {},
		setHotkeyBinding: vi.fn(),
		getSkipNextDefaultModelRefresh: () => false,
		clearSkipNextDefaultModelRefresh: vi.fn(),
		refreshDefaultModelFromBackend: vi.fn(),
	};
}

function makeRegistrationPort(registration: ChatEventRegistration) {
	const maps: Array<Parameters<ChatEventRegistrationPort>[0]> = [];
	const tags: string[] = [];
	const registerPort: ChatEventRegistrationPort = (listeners, options) => {
		maps.push(listeners);
		tags.push(options.tag);
		return registration;
	};
	return { registerPort, maps, tags };
}

function deferredReady() {
	let resolve!: () => void;
	const promise = new Promise<void>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

describe('createChatEventController', () => {
	beforeEach(() => {
		listen.mockReset();
	});

	it('composes session, app, agent, and usage listener channels', async () => {
		const registration = { ready: Promise.resolve(), dispose: vi.fn() };
		const { registerPort, maps, tags } = makeRegistrationPort(registration);
		const controller = createChatEventController(makeDependencies(), registerPort);

		await controller.register();

		expect(tags).toEqual(['+page']);
		expect(Object.keys(maps[0]).sort()).toEqual(
			[
				'session:lifecycle',
				'hotkey:rebind',
				'llm:config_changed',
				'agent:thought',
				'agent:thought_chunk',
				'agent:reasoning_chunk',
				'agent:stream_reset',
				'agent:web_search',
				'agent:supplement',
				'agent:tool_call',
				'agent:tool_call_chunk',
				'agent:tool_output',
				'agent:observation',
				'agent:usage',
				'agent:compaction',
				'agent:media_plan',
			].sort(),
		);
		expect(listen).not.toHaveBeenCalled();
	});

	it('returns the same ready promise and waits for registration', async () => {
		const readiness = deferredReady();
		const registration = { ready: readiness.promise, dispose: vi.fn() };
		const { registerPort, maps } = makeRegistrationPort(registration);
		const controller = createChatEventController(makeDependencies(), registerPort);
		let registered = false;

		const firstReady = controller.register();
		const secondReady = controller.register();
		expect(secondReady).toBe(firstReady);
		expect(maps).toHaveLength(1);
		void firstReady.then(() => {
			registered = true;
		});
		await Promise.resolve();

		expect(registered).toBe(false);
		readiness.resolve();
		await firstReady;
		await secondReady;
		expect(registered).toBe(true);
	});

	it('disposes the registration only once', async () => {
		const registration = { ready: Promise.resolve(), dispose: vi.fn() };
		const { registerPort } = makeRegistrationPort(registration);
		const controller = createChatEventController(makeDependencies(), registerPort);
		await controller.register();

		controller.dispose();
		controller.dispose();

		expect(registration.dispose).toHaveBeenCalledTimes(1);
	});

	it('disposes before ready so the registration port can release late subscriptions', async () => {
		const readiness = deferredReady();
		let disposed = false;
		const lateUnlisten = vi.fn();
		const registration: ChatEventRegistration = {
			ready: readiness.promise.then(() => {
				// events.ts immediately invokes unlisten handles that resolve after
				// its registration handle has been disposed.
				if (disposed) lateUnlisten();
			}),
			dispose: vi.fn(() => {
				disposed = true;
			}),
		};
		const { registerPort } = makeRegistrationPort(registration);
		const controller = createChatEventController(makeDependencies(), registerPort);
		const ready = controller.register();

		controller.dispose();
		controller.dispose();
		readiness.resolve();
		await ready;

		expect(registration.dispose).toHaveBeenCalledTimes(1);
		expect(lateUnlisten).toHaveBeenCalledTimes(1);
	});
});
