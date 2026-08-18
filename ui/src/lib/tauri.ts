let _tauriInvoke: ((cmd: string, args?: any) => Promise<any>) | null = null;
let _tauriListen: ((event: string, handler: (event: unknown) => void) => Promise<unknown>) | null = null;
let _initialized = false;
let _logger: { debug: (c: string, m: string, ...a: unknown[]) => void; info: (c: string, m: string, ...a: unknown[]) => void; warn: (c: string, m: string, ...a: unknown[]) => void; error: (c: string, m: string, ...a: unknown[]) => void } | null = null;

const isTauri = () => {
	const w = typeof window !== 'undefined' ? (window as any) : null;
	return !!w && !!(w.__TAURI_INTERNALS__ || w.__TAURI__);
};

async function loadLogger() {
	if (_logger) return _logger;
	try {
		const mod = await import('./logger.ts');
		_logger = mod.default;
	} catch {
		_logger = null;
	}
	return _logger;
}

async function init() {
	if (_initialized) return;
	if (!isTauri()) return;
	try {
		const mod = await import('@tauri-apps/api/core');
		_tauriInvoke = mod.invoke;
	} catch (e) {
		const log = await loadLogger();
		log?.warn('tauri', '@tauri-apps/api/core import failed', e);
		return;
	}
	try {
		const mod = await import('@tauri-apps/api/event');
		_tauriListen = mod.listen;
	} catch (e) {
		const log = await loadLogger();
		log?.warn('tauri', '@tauri-apps/api/event import failed', e);
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
			const log = await loadLogger();
			log?.error('invoke', `'${cmd}' failed`, e);
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
		const unlisten = await _tauriListen(event, handler);
		return () => {
			if (typeof unlisten === 'function') unlisten();
		};
	}
	return () => {};
}
