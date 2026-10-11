/** Generic event envelope emitted by Tauri's renderer listener API. */
export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}

/** Validate the renderer-local Tauri event sequence id at every event boundary. */
export function isValidTauriEventId(value: unknown): value is number {
	return Number.isSafeInteger(value) && (value as number) >= 0;
}
