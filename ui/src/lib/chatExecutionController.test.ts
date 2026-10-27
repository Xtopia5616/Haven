import { describe, expect, it, vi } from 'vitest';
import { createChatExecutionController } from './chatExecutionController.ts';
import { SessionReducer } from './sessionReducer.ts';

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((yes, no) => {
		resolve = yes;
		reject = no;
	});
	return { promise, resolve, reject };
}

function harness(sessionId: string | null = 'session-a') {
	const reducer = new SessionReducer();
	reducer.dispatch({ type: 'session/selected', sessionId });
	const deps = {
		getSelection: () => ({
			sessionId: reducer.snapshot().activeSessionId,
			version: reducer.currentUserSelectionVersion(),
		}),
		setWorkspace: vi.fn(async (_id: string, root: string | null) => root),
		setMode: vi.fn(
			async (_id: string, mode: 'collaborative' | 'analyze' | 'unattended') => mode,
		),
		setDefaultWorkspace: vi.fn(async () => {}),
		setDefaultMode: vi.fn(async () => {}),
		pickWorkspace: vi.fn(async (): Promise<string | null> => null),
		applyWorkspace: vi.fn(),
		applyMode: vi.fn(),
		applyDefaultWorkspace: vi.fn(),
		applyDefaultMode: vi.fn(),
		rememberWorkspace: vi.fn(),
		invalidateWorkspaceRead: vi.fn(),
		invalidateModeRead: vi.fn(),
		notify: vi.fn(),
	};
	return {
		deps,
		controller: createChatExecutionController(deps),
		select: (id: string | null) => {
			reducer.beginUserSelection();
			reducer.dispatch({ type: 'session/selected', sessionId: id });
		},
	};
}

describe('chat execution selection ownership', () => {
	it('does not apply an old workspace response after selecting another session', async () => {
		const h = harness();
		const pending = deferred<string | null>();
		h.deps.setWorkspace.mockReturnValue(pending.promise);
		const action = h.controller.workspace('D:\\A');
		h.select('session-b');
		pending.resolve('D:\\A');
		await action;
		expect(h.deps.setWorkspace).toHaveBeenCalledWith('session-a', 'D:\\A');
		expect(h.deps.applyWorkspace).not.toHaveBeenCalled();
		expect(h.deps.notify).not.toHaveBeenCalled();
	});
	it('does not apply an old mode after switching away and back', async () => {
		const h = harness();
		const pending = deferred<'analyze'>();
		h.deps.setMode.mockReturnValue(pending.promise);
		const action = h.controller.mode('analyze');
		h.select('session-b');
		h.select('session-a');
		pending.resolve('analyze');
		await action;
		expect(h.deps.applyMode).not.toHaveBeenCalled();
		expect(h.deps.notify).not.toHaveBeenCalled();
	});
	it('discards a directory picker result after a new selection', async () => {
		const h = harness();
		const pending = deferred<string | null>();
		h.deps.pickWorkspace.mockReturnValue(pending.promise);
		const action = h.controller.pickWorkspace();
		h.select('session-b');
		pending.resolve('D:\\A');
		await action;
		expect(h.deps.setWorkspace).not.toHaveBeenCalled();
		expect(h.deps.setDefaultWorkspace).not.toHaveBeenCalled();
	});
	it('updates the persisted default without overwriting a later active session', async () => {
		const h = harness(null);
		const pending = deferred<void>();
		h.deps.setDefaultWorkspace.mockReturnValue(pending.promise);
		const action = h.controller.workspace('D:\\Default');
		h.select('session-b');
		pending.resolve();
		await action;
		expect(h.deps.applyDefaultWorkspace).toHaveBeenCalledWith('D:\\Default');
		expect(h.deps.applyWorkspace).not.toHaveBeenCalled();
	});
	it('updates the default mode without overwriting a later active session', async () => {
		const h = harness(null);
		const pending = deferred<void>();
		h.deps.setDefaultMode.mockReturnValue(pending.promise);
		const action = h.controller.mode('analyze');
		h.select('session-b');
		pending.resolve();
		await action;
		expect(h.deps.applyDefaultMode).toHaveBeenCalledWith('analyze');
		expect(h.deps.applyMode).not.toHaveBeenCalled();
		expect(h.deps.notify).not.toHaveBeenCalled();
	});
	it('applies canonical saved values to the unchanged selection and invalidates old reads', async () => {
		const h = harness();
		h.deps.setWorkspace.mockResolvedValue('D:\\Canonical');
		await h.controller.workspace('D:/Canonical/');
		await h.controller.mode('analyze');
		expect(h.deps.invalidateWorkspaceRead).toHaveBeenCalledOnce();
		expect(h.deps.invalidateModeRead).toHaveBeenCalledOnce();
		expect(h.deps.applyWorkspace).toHaveBeenCalledWith('D:\\Canonical');
		expect(h.deps.applyMode).toHaveBeenCalledWith('analyze');
		expect(h.deps.notify).toHaveBeenCalledTimes(2);
	});
	it('does not change displayed or remembered workspace when saving fails', async () => {
		const h = harness();
		const error = new Error('save rejected');
		h.deps.setWorkspace.mockRejectedValue(error);
		await expect(h.controller.workspace('D:\\A')).rejects.toBe(error);
		expect(h.deps.applyWorkspace).not.toHaveBeenCalled();
		expect(h.deps.rememberWorkspace).not.toHaveBeenCalled();
		expect(h.deps.notify).not.toHaveBeenCalled();
	});
	it('leaves persistence unchanged when the picker is cancelled', async () => {
		const h = harness();
		await h.controller.pickWorkspace();
		expect(h.deps.setWorkspace).not.toHaveBeenCalled();
		expect(h.deps.invalidateWorkspaceRead).not.toHaveBeenCalled();
	});
});
