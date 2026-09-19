export interface SessionRefreshScheduler {
	refresh: () => Promise<void>;
	schedule: () => void;
	dispose: () => void;
}

/**
 * Coalesce history refreshes without allowing an in-flight query to be
 * invalidated by a burst of lifecycle events. A request that arrives while a
 * query is active schedules exactly one follow-up query after that query
 * settles; disposing the view cancels both the timer and the follow-up.
 */
export function createSessionRefreshScheduler(
	refreshFn: () => Promise<void>,
	delayMs = 300,
	timers: Pick<typeof globalThis, 'setTimeout' | 'clearTimeout'> = globalThis,
): SessionRefreshScheduler {
	let timer: ReturnType<typeof setTimeout> | null = null;
	let active: Promise<void> | null = null;
	let queued = false;
	let disposed = false;

	function finish(request: Promise<void>) {
		if (active !== request) return;
		active = null;
		if (queued && !disposed) {
			if (timer) timers.clearTimeout(timer);
			timer = null;
			queued = false;
			void refresh();
		} else {
			queued = false;
		}
	}

	function refresh(): Promise<void> {
		if (disposed) return Promise.resolve();
		if (active) {
			queued = true;
			return active;
		}
		const request = Promise.resolve().then(refreshFn);
		active = request;
		request.then(
			() => finish(request),
			() => finish(request),
		);
		return request;
	}

	function schedule() {
		if (disposed) return;
		if (timer) timers.clearTimeout(timer);
		timer = timers.setTimeout(() => {
			timer = null;
			void refresh();
		}, delayMs);
	}

	function dispose() {
		disposed = true;
		queued = false;
		if (timer) timers.clearTimeout(timer);
		timer = null;
	}

	return { refresh, schedule, dispose };
}
