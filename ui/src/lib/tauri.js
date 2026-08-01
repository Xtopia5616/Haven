let _tauriInvoke = null;
let _tauriListen = null;
let _initialized = false;
let _logger = null;

const isTauri = () => typeof window !== 'undefined' && !!(/** @type {any} */ (window).__TAURI_INTERNALS__ || /** @type {any} */ (window).__TAURI__);

async function loadLogger() {
	if (_logger) return _logger;
	try {
		const mod = await import('./logger.js');
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
		console.warn('[tauri] @tauri-apps/api/core import failed:', e);
		return;
	}
	try {
		const mod = await import('@tauri-apps/api/event');
		_tauriListen = mod.listen;
	} catch (e) {
		console.warn('[tauri] @tauri-apps/api/event import failed:', e);
		return;
	}
	_initialized = true;
}

export async function invoke(cmd, args) {
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

export async function listen(event, handler) {
	await init();
	if (isTauri() && _tauriListen) {
		return _tauriListen(event, handler);
	}
	return () => {};
}
