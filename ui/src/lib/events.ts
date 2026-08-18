import { listen } from './tauri.js';
import logger from './logger.js';

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
export function registerListeners(map, { tag = 'unknown' } = {}) {
	/** @type {Array<() => void>} */
	const unlisteners = [];
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
 * Register a single Tauri event listener with the same fail-safe semantics as
 * registerListeners. Returns a handle whose `dispose()` unregisters it.
 *
 * @param {string} event
 * @param {(event: any) => void} handler
 * @param {{ tag?: string }} [opts]
 * @returns {Promise<{ dispose: () => void }>}
 */
export async function registerOne(event, handler, { tag = 'unknown' } = {}) {
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
