import { listen } from './tauri.ts';
import logger from './logger.ts';
import {
	mapSessionEvent,
	type SessionEventName,
	type SessionEventPayloadMap,
	type TauriEvent,
} from './contracts/session.ts';

type SessionListenerMap = Partial<{
	[K in SessionEventName]: (event: TauriEvent<SessionEventPayloadMap[K]>) => void;
}>;

/**
 * Register many Tauri event listeners from a single map and return a handle
 * that can dispose them all. Listener registration failures are logged and
 * swallowed so a failing registration never blocks the caller's mount.
 *
 *   const events = registerListeners({
 *     'session:created': (event) => { ... },
 *     'session:updated': (event) => { ... },
 *   }, { tag: '+layout' });
 *   onMount(async () => { await events.ready; ... });
 *   onDestroy(() => events.dispose());
 *
 * @param {Record<string, (event: any) => void>} map
 * @param {{ tag?: string }} [opts]
 * @returns {{ ready: Promise<void>, dispose: () => void }}
 */
export function registerListeners(
	map: Record<string, (event: any) => void>,
	{ tag = 'unknown' }: { tag?: string } = {},
): { ready: Promise<void>; dispose: () => void } {
	/** @type {Array<() => void>} */
	const unlisteners: Array<() => void> = [];
	let disposed = false;
	// Promise.all resolves to `void[]`; convert to a plain `Promise<void>` so
	// callers can `await` it without a stray array type leaking out.
	const ready = Promise.all(
		Object.entries(map).map(async ([event, handler]) => {
			try {
				const unsub = await listen(event, handler);
				if (disposed) {
					unsub();
				} else {
					unlisteners.push(unsub);
				}
			} catch (e) {
				logger.error(tag, `Failed to register listener for '${event}'`, e);
			}
		})
	).then(() => {});
	return {
		ready,
		dispose() {
			disposed = true;
			const pending = unlisteners.splice(0);
			pending.forEach((u) => {
				try {
					u();
				} catch {
					// unlisten cleanup must never throw into onDestroy
				}
			});
		},
	};
}

/**
 * Adapt session event payloads at the Tauri boundary. Routes and views must
 * consume the camelCase payloads from `contracts/session.ts`, never raw
 * snake_case wire fields.
 */
export function sessionEventListeners(
	map: SessionListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				const name = eventName as SessionEventName;
				handler?.(mapSessionEvent({ ...event, event: name } as never) as never);
			},
		]),
	);
}

/**
 * Register a single Tauri event listener with the same fail-safe semantics as
 * registerListeners. Returns a handle whose `dispose()` unregisters it.
 *
 * @param {string} event
 * @param {(event: any) => void} handler
 * @param {{ tag?: string }} [opts]
 * @returns {Promise<{ dispose: () => void }>}
 */
export async function registerOne(
	event: string,
	handler: (event: any) => void,
	{ tag = 'unknown' }: { tag?: string } = {},
): Promise<{ dispose: () => void }> {
	try {
		const unsub = await listen(event, handler);
		return {
			dispose() {
				try {
					unsub();
				} catch {
					// ignore
				}
			},
		};
	} catch (e) {
		logger.error(tag, `Failed to register listener for '${event}'`, e);
		return { dispose() {} };
	}
}

/** Register one typed session listener with the same safe cleanup semantics. */
export async function registerSessionListener<K extends SessionEventName>(
	event: K,
	handler: (event: TauriEvent<SessionEventPayloadMap[K]>) => void,
	{ tag = 'unknown' }: { tag?: string } = {},
): Promise<{ dispose: () => void }> {
	return registerOne(
		event,
		(rawEvent) => handler(mapSessionEvent({ ...rawEvent, event } as never)),
		{ tag },
	);
}
