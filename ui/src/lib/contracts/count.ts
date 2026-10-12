import { isSafeUnsignedInteger } from './valueGuards.ts';

/** Validate an IPC count before exposing a Rust integer as a JavaScript number. */
export function parseCountResponse(value: unknown, command: string): number {
	if (!isSafeUnsignedInteger(value)) {
		throw new Error(`Invalid ${command} count response`);
	}
	return value;
}
