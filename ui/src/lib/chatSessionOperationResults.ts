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
