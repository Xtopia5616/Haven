import logger from './logger.ts';
import { logError } from './errorHandling.ts';
import { formatError } from './formatError.ts';

let _tauriInvoke: ((cmd: string, args?: any) => Promise<any>) | null = null;
let _tauriListen: ((event: string, handler: (event: unknown) => void) => Promise<unknown>) | null =
	null;
let _initialized = false;

/** True when running inside a Tauri webview (not plain browser / SSR). */
export const isTauri = () => {
	const w = typeof window !== 'undefined' ? (window as any) : null;
	return !!w && !!(w.__TAURI_INTERNALS__ || w.__TAURI__);
};

async function init() {
	if (_initialized) return;
	if (!isTauri()) return;
	try {
		const mod = await import('@tauri-apps/api/core');
		_tauriInvoke = mod.invoke;
	} catch (e) {
		logger.warn('tauri', '@tauri-apps/api/core import failed', e);
		return;
	}
	try {
		const mod = await import('@tauri-apps/api/event');
		_tauriListen = mod.listen;
	} catch (e) {
		logger.warn('tauri', '@tauri-apps/api/event import failed', e);
		return;
	}
	_initialized = true;
}

export async function invoke(cmd: string, args?: unknown): Promise<any> {
	await init();
	if (isTauri() && _tauriInvoke) {
		try {
			return await _tauriInvoke(cmd, args);
		} catch (e) {
			logError('invoke', `command '${cmd}' failed`, e);
			throw e;
		}
	}
	throw new Error(`Tauri not available, cannot invoke '${cmd}'`);
}

export async function listen(
	event: string,
	handler: (event: unknown) => void,
): Promise<() => void> {
	await init();
	if (isTauri() && _tauriListen) {
		try {
			const unlisten = await _tauriListen(event, handler);
			return () => {
				if (typeof unlisten !== 'function') return;
				try {
					const result = unlisten();
					if (result && typeof (result as Promise<unknown>).catch === 'function') {
						(result as Promise<unknown>).catch((error) =>
							logger.warn('tauri', `unlisten '${event}' failed`, formatError(error)),
						);
					}
				} catch (error) {
					logger.warn('tauri', `unlisten '${event}' failed`, formatError(error));
				}
			};
		} catch (error) {
			logError('listen', `event '${event}' registration failed`, error);
			throw error;
		}
	}
	if (isTauri()) throw new Error(`Tauri event API not available, cannot listen '${event}'`);
	return () => {};
}
