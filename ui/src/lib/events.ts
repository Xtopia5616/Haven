import { listen } from './tauri.ts';
import logger from './logger.ts';
import {
	mapSessionEvent,
	type SessionLifecyclePayload,
} from './contracts/session.ts';
import type { TauriEvent } from './contracts/tauriEvent.ts';
import {
	mapToolRunEvent,
	type ToolRunEventName,
	type ToolRunPayload,
} from './contracts/toolRun.ts';
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
import { mapAppEvent, type AppEventName, type AppEventPayloadMap } from './contracts/app.ts';

type SessionLifecycleListener = (event: TauriEvent<SessionLifecyclePayload>) => void;

type ToolRunListenerMap = Partial<{
	[K in ToolRunEventName]: (event: TauriEvent<ToolRunPayload>) => void;
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

function adaptSessionEvent(event: TauriEvent<unknown>): TauriEvent<SessionLifecyclePayload> | null {
	const mapped = mapSessionEvent(event);
	if (!mapped) {
		logger.warn('events', `Dropping malformed payload for '${event.event}'`);
	}
	return mapped;
}

function adaptToolRunEvent<K extends ToolRunEventName>(
	eventName: K,
	event: TauriEvent<unknown>,
): TauriEvent<ToolRunPayload> | null {
	const mapped = mapToolRunEvent({ ...event, event: eventName });
	if (!mapped) {
		logger.warn('events', `Dropping malformed payload for '${eventName}'`);
	}
	return mapped;
}

function adaptAppEvent<K extends AppEventName>(
	eventName: K,
	event: TauriEvent<unknown>,
): TauriEvent<AppEventPayloadMap[K]> | null {
	const mapped = mapAppEvent({ ...event, event: eventName });
	if (!mapped) {
		logger.warn('events', `Dropping malformed payload for '${eventName}'`);
	}
	return mapped;
}

function adaptAgentEvent<K extends AgentEventName>(
	eventName: K,
	event: TauriEvent<unknown>,
): TauriEvent<AgentEventPayloadMap[K]> | null {
	const mapped = mapAgentEvent({ ...event, event: eventName });
	if (!mapped) {
		logger.warn('events', `Dropping malformed payload for '${eventName}'`);
	}
	return mapped;
}

/**
 * Register many Tauri event listeners from a single map and return a handle
 * that can dispose them all. Listener registration failures are logged and
 * swallowed so a failing registration never blocks the caller's mount.
 *
 *   const events = registerListeners({
 *     'session:lifecycle': (event) => { ... },
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
		}),
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
	handler: SessionLifecycleListener,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return {
		'session:lifecycle': (event) => {
			protectEventCallback('session:lifecycle', () => {
				const mapped = adaptSessionEvent(event);
				if (mapped) handler(mapped);
			});
		},
	};
}

/**
 * Adapt ToolRun event payloads at the Tauri boundary. Routes and stores consume
 * the named camelCase ToolRun DTO, never the tool crate's internal JSON shape.
 */
export function toolRunEventListeners(
	map: ToolRunListenerMap,
): Record<string, (event: TauriEvent<unknown>) => void> {
	return Object.fromEntries(
		Object.entries(map).map(([eventName, handler]) => [
			eventName,
			(event: TauriEvent<unknown>) => {
				protectEventCallback(eventName, () => {
					const name = eventName as ToolRunEventName;
					const mapped = adaptToolRunEvent(name, event);
					if (mapped) handler?.(mapped as never);
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
					const name = eventName as AgentEventName;
					const mapped = adaptAgentEvent(name, event);
					if (mapped) handler?.(mapped as never);
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
					const name = eventName as AppEventName;
					const mapped = adaptAppEvent(name, event);
					if (mapped) handler?.(mapped as never);
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

/** Register the one typed session lifecycle listener with safe cleanup semantics. */
export async function registerSessionLifecycleListener(
	handler: SessionLifecycleListener,
	{ tag = 'unknown' }: { tag?: string } = {},
): Promise<{ dispose: () => void }> {
	return registerOne(
		'session:lifecycle',
		(rawEvent) =>
			protectEventCallback('session:lifecycle', () => {
				const mapped = adaptSessionEvent(rawEvent);
				if (mapped) handler(mapped);
			}),
		{ tag },
	);
}

/** Register one typed app listener through the shared app event mapper. */
export async function registerAppListener<K extends AppEventName>(
	event: K,
	handler: (event: TauriEvent<AppEventPayloadMap[K]>) => void,
	{ tag = 'unknown' }: { tag?: string } = {},
): Promise<{ dispose: () => void }> {
	return registerOne(
		event,
		(rawEvent) =>
			protectEventCallback(event, () => {
				const mapped = adaptAppEvent(event, rawEvent as TauriEvent<unknown>);
				if (mapped) handler(mapped);
			}),
		{ tag },
	);
}
