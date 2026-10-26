import type { SessionForkResult, SessionSwitchResult } from './chatSessionController.ts';

export type ChatSessionSubmitResult =
	{ kind: 'submitted'; sessionId: string | null } | { kind: 'failed'; error: unknown };

export type ChatSessionSubmitOutcome =
	| {
			kind: 'submitted';
			sessionId: string | null;
			refresh: { kind: 'complete' } | { kind: 'failed'; error: unknown };
	  }
	| { kind: 'failed'; error: unknown };

/** Keep transcript acceptance distinct from a later session-list refresh failure. */
export async function submitAndRefreshSessionList(
	submit: () => Promise<ChatSessionSubmitResult>,
	refreshSessionList: () => Promise<void>,
): Promise<ChatSessionSubmitOutcome> {
	const submission = await submit();
	if (submission.kind === 'failed') return submission;

	try {
		await refreshSessionList();
		return { ...submission, refresh: { kind: 'complete' } };
	} catch (error) {
		return { ...submission, refresh: { kind: 'failed', error } };
	}
}

export type ChatSessionForkOutcome =
	| Exclude<SessionForkResult, { kind: 'created' }>
	| (Extract<SessionForkResult, { kind: 'created' }> & {
			refresh: { kind: 'complete' } | { kind: 'failed'; error: unknown };
			selection: SessionSwitchResult;
	  });

/** Refresh and select after creation without reclassifying the persisted child as failed. */
export async function forkAndRefreshSessionList(
	create: () => Promise<SessionForkResult>,
	refreshSessionList: () => Promise<void>,
	select: (fork: Extract<SessionForkResult, { kind: 'created' }>) => Promise<SessionSwitchResult>,
): Promise<ChatSessionForkOutcome> {
	const fork = await create();
	if (fork.kind !== 'created') return fork;
	let refresh: { kind: 'complete' } | { kind: 'failed'; error: unknown };
	try {
		await refreshSessionList();
		refresh = { kind: 'complete' };
	} catch (error) {
		refresh = { kind: 'failed', error };
	}
	return { ...fork, refresh, selection: await select(fork) };
}
