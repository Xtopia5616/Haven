import logger from './logger.ts';
import { logError } from './errorHandling.ts';
import { formatError, isAuthorizationConfirmationPending } from './formatError.ts';
import type {
	TauriCommandName,
	TauriCommandRequestArgs,
	TauriCommandResponse,
} from './contracts/generatedCommands.ts';

type RawTauriInvoke = (cmd: string, args?: unknown) => Promise<unknown>;

let _tauriInvoke: RawTauriInvoke | null = null;
let _tauriListen: ((event: string, handler: (event: unknown) => void) => Promise<unknown>) | null =
	null;
let _initialized = false;

/** True when running inside a Tauri webview (not plain browser / SSR). */
export const isTauri = () => {
	const w =
		typeof window !== 'undefined'
			? (window as Window & { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown })
			: null;
	return !!w && !!(w.__TAURI_INTERNALS__ || w.__TAURI__);
};

async function init() {
	if (_initialized) return;
	if (!isTauri()) return;
	try {
		const mod = await import('@tauri-apps/api/core');
		_tauriInvoke = (command, args) =>
			mod.invoke<unknown>(command, args as Record<string, unknown> | undefined);
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

/**
 * Typed command boundary generated from Rust Tauri handler signatures. The
 * returned value remains untrusted wire data; callers with structured results
 * must keep using their existing runtime parser/validator.
 */
export async function invoke<K extends TauriCommandName>(
	cmd: K,
	...args: TauriCommandRequestArgs<K>
): Promise<TauriCommandResponse<K>> {
	return (await invokeUnknown(cmd, args[0])) as TauriCommandResponse<K>;
}

async function invokeUnknown(cmd: string, args?: unknown): Promise<unknown> {
	await init();
	if (!isTauri() || !_tauriInvoke) {
		const error = new Error(`Tauri not available, cannot invoke '${cmd}'`);
		logError('invoke', `command '${cmd}' failed`, error);
		throw error;
	}
	try {
		return await _tauriInvoke(cmd, args);
	} catch (error) {
		if (isAuthorizationConfirmationPending(error)) {
			// The confirmation request is already represented by the interaction
			// event; this rejected invoke is only a control-flow signal.
		} else if (formatError(error) === 'Confirmation request is stale or already resolved') {
			logger.warn('invoke', `command '${cmd}' ended with a stale confirmation`, error);
		} else {
			logError('invoke', `command '${cmd}' failed`, error);
		}
		throw error;
	}
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
