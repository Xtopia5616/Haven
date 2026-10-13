import logger from './logger.ts';
import { logError } from './errorHandling.ts';
import { formatError, isAuthorizationConfirmationPending } from './formatError.ts';
import type {
	TauriCommandName,
	TauriCommandRequestArgs,
	TauriCommandResponse,
} from './contracts/generatedCommands.ts';
import { TAURI_COMMAND_RESPONSE_SHAPES } from './contracts/generatedCommands.ts';
import { findGeneratedWireShapeMismatch } from './contracts/generatedWireShape.ts';
import type { TauriEvent } from './contracts/tauriEvent.ts';

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
	} catch {
		logger.warn('tauri', '@tauri-apps/api/core import failed');
		return;
	}
	try {
		const mod = await import('@tauri-apps/api/event');
		_tauriListen = mod.listen;
	} catch {
		logger.warn('tauri', '@tauri-apps/api/event import failed');
		return;
	}
	_initialized = true;
}

/**
 * Typed command boundary generated from Rust Tauri handler signatures. The
 * returned value is checked against the complete Rust-generated wire shape.
 * Domain adapters still validate identity, cross-field, and product semantics.
 */
export async function invoke<K extends TauriCommandName>(
	cmd: K,
	...args: TauriCommandRequestArgs<K>
): Promise<TauriCommandResponse<K>> {
	const response = await invokeUnknown(cmd, args[0]);
	const mismatch = findGeneratedWireShapeMismatch(response, TAURI_COMMAND_RESPONSE_SHAPES[cmd]);
	if (mismatch) {
		const error = new Error(`Invalid Tauri response for '${cmd}' at ${mismatch}`);
		logError('invoke', error);
		throw error;
	}
	return response as TauriCommandResponse<K>;
}

async function invokeUnknown(cmd: string, args?: unknown): Promise<unknown> {
	await init();
	if (!isTauri() || !_tauriInvoke) {
		const error = new Error(`Tauri not available, cannot invoke '${cmd}'`);
		logError('invoke', error);
		throw error;
	}
	try {
		return await _tauriInvoke(cmd, args);
	} catch (error) {
		if (isAuthorizationConfirmationPending(error)) {
			// The confirmation request is already represented by the interaction
			// event; this rejected invoke is only a control-flow signal.
		} else if (formatError(error) === 'Confirmation request is stale or already resolved') {
			logger.warn('invoke', 'command ended with a stale confirmation');
		} else {
			// Tauri can reject with a primitive string. Normalize it once so a
			// caller's reportError catch can recognize that invoke already logged it.
			const reportedError =
				error && typeof error === 'object' ? error : new Error(formatError(error));
			logError('invoke', reportedError);
			throw reportedError;
		}
		throw error;
	}
}

export async function listen(
	event: string,
	handler: (event: TauriEvent<unknown>) => void,
): Promise<() => void> {
	await init();
	if (isTauri() && _tauriListen) {
		const unlisten = await _tauriListen(event, (rawEvent) => {
			handler(rawEvent as TauriEvent<unknown>);
		});
		return () => {
			if (typeof unlisten !== 'function') return;
			try {
				const result = unlisten();
				if (result && typeof (result as Promise<unknown>).catch === 'function') {
					(result as Promise<unknown>).catch(() =>
						logger.warn('tauri', 'event listener cleanup failed'),
					);
				}
			} catch {
				logger.warn('tauri', 'event listener cleanup failed');
			}
		};
	}
	if (isTauri()) throw new Error(`Tauri event API not available, cannot listen '${event}'`);
	return () => {};
}
