export const BOOTSTRAP_PROBE_INTERVAL_MS = 1000;
const BOOTSTRAP_PROBE_MAX_INTERVAL_MS = 10000;

/** Return true only for the backend's terminal bootstrap state. */
export function isBootstrapReady(status: unknown): status is 'ready' {
	return status === 'ready';
}

/** Back off failed IPC probes without delaying normal loading-state checks. */
export function nextBootstrapProbeInterval(failureStreak: number): number {
	const streak = Math.max(0, Math.min(Math.floor(failureStreak), 4));
	return Math.min(BOOTSTRAP_PROBE_INTERVAL_MS * 2 ** streak, BOOTSTRAP_PROBE_MAX_INTERVAL_MS);
}
