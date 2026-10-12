/**
 * Drop renderer-only dismissal state once an interaction is no longer pending.
 * Returns null when the current set already contains only live request IDs.
 */
export function retainPendingDismissedRequestIds(
	dismissedRequestIds: ReadonlySet<string>,
	pendingRequestIds: Iterable<string>,
): Set<string> | null {
	const pendingIds = new Set(pendingRequestIds);
	const retainedIds = [...dismissedRequestIds].filter((requestId) => pendingIds.has(requestId));
	return retainedIds.length === dismissedRequestIds.size ? null : new Set(retainedIds);
}
