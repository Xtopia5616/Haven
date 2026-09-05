import { listen } from './tauri.ts';
import logger from './logger.ts';
import {
	mapSessionEvent,
	type SessionEventName,
	type SessionEventPayloadMap,
	type TauriEvent,
} from './contracts/session.ts';
import {
	mapActionEvent,
	type ActionEventName,
	type ActionPayload,
} from './contracts/action.ts';
import {
	mapRecordingEvent,
	type RecordingEventName,
	type RecordingEventPayloadMap,
} from './contracts/recording.ts';
import {
	mapAgentEvent,
	type AgentEventName,
	type AgentEventPayloadMap,
} from './contracts/agent.ts';
import {
	mapAppEvent,
	type AppEventName,
	type AppEventPayloadMap,
} from './contracts/app.ts';

type SessionListenerMap = Partial<{
	[K in SessionEventName]: (event: TauriEvent<SessionEventPayloadMap[K]>) => void;
}>;

type ActionListenerMap = Partial<{
	[K in ActionEventName]: (event: TauriEvent<ActionPayload>) => void;
}>;

type RecordingListenerMap = Partial<{
	[K in RecordingEventName]: (event: TauriEvent<RecordingEventPayloadMap[K]>) => void;
}>;

type AgentListenerMap = Partial<{
	[K in AgentEventName]: (event: TauriEvent<AgentEventPayloadMap[K]>) => void;
}>;

type AppListenerMap = Partial<{
	[K in AppEventName]: (event: TauriEvent<AppEventPayloadMap[K]>) => void;
}>;

/** Keep malformed payloads and listener exceptions isolated from the event bus. */
function protectEventCallback(eventName: string, callback: () => unknown): void {
	try {
		void Promise.resolve(callback()).catch((error) => {
			logger.error('events', `Event handler failed for '${eventName}'`, error);
		});
	} catch (error) {
		logger.error('events', `Event mapping failed for '${eventName}'`, error);
	}
}

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
				protectEventCallback(eventName, () => {
					const name = eventName as SessionEventName;
					handler?.(mapSessionEvent({ ...event, event: name } as never) as never);
				});
			},
		]),
	);
}

/**
 * Adapt action event payloads at the Tauri boundary. Routes and stores consume
 * the named camelCase action DTO, never the tool crate's internal JSON shape.
 */
export function actionEventListeners(
	map: ActionListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				protectEventCallback(eventName, () => {
					handler?.(mapActionEvent({ ...event, event: eventName } as never) as never);
				});
			},
		]),
	);
}

/** Adapt recording events once, before routes consume their camelCase DTOs. */
export function recordingEventListeners(
	map: RecordingListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				protectEventCallback(eventName, () => {
					handler?.(mapRecordingEvent({ ...event, event: eventName } as never) as never);
				});
			},
		]),
	);
}

/** Adapt agent events once, before routes consume their camelCase DTOs. */
export function agentEventListeners(
	map: AgentListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				protectEventCallback(eventName, () => {
					handler?.(mapAgentEvent({ ...event, event: eventName } as never) as never);
				});
			},
		]),
	);
}

/** Adapt app-shell events once, before routes consume their camelCase DTOs. */
export function appEventListeners(
	map: AppListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				protectEventCallback(eventName, () => {
					handler?.(mapAppEvent({ ...event, event: eventName } as never) as never);
				});
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
		(rawEvent) =>
			protectEventCallback(event, () => handler(mapSessionEvent({ ...rawEvent, event } as never))),
		{ tag },
	);
}
