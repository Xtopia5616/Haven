import {
	BOOTSTRAP_STATUS_VALUES,
	type BootstrapStatus,
} from './contracts/generatedCommands.ts';
import { isOneOf } from './contracts/valueGuards.ts';

export const BOOTSTRAP_PROBE_INTERVAL_MS = 1000;
const BOOTSTRAP_PROBE_MAX_INTERVAL_MS = 10000;

/** Validate the command response against the backend-owned closed status enum. */
export function parseBootstrapStatus(value: unknown): BootstrapStatus {
	if (!isOneOf(value, BOOTSTRAP_STATUS_VALUES)) {
		throw new TypeError('Unknown bootstrap status response');
	}
	return value;
}

/** Return true only for the backend's terminal bootstrap state. */
export function isBootstrapReady(status: unknown): status is Extract<BootstrapStatus, 'ready'> {
	return status === 'ready';
}

/** Back off failed IPC probes without delaying normal loading-state checks. */
export function nextBootstrapProbeInterval(failureStreak: number): number {
	const streak = Math.max(0, Math.min(Math.floor(failureStreak), 4));
	return Math.min(BOOTSTRAP_PROBE_INTERVAL_MS * 2 ** streak, BOOTSTRAP_PROBE_MAX_INTERVAL_MS);
}
