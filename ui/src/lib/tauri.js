let _tauriInvoke = null;
let _tauriListen = null;
let _initialized = false;

const isTauri = () => typeof window !== 'undefined' && !!(/** @type {any} */ (window).__TAURI_INTERNALS__ || /** @type {any} */ (window).__TAURI__);

async function init() {
	if (_initialized) return;
	_initialized = true;
	try {
		const mod = await import('@tauri-apps/api/core');
		_tauriInvoke = mod.invoke;
	} catch {
		/* not in Tauri */
	}
	try {
		const mod = await import('@tauri-apps/api/event');
		_tauriListen = mod.listen;
	} catch {
		/* not in Tauri */
	}
}

export async function invoke(cmd, args) {
	await init();
	if (isTauri() && _tauriInvoke) {
		return _tauriInvoke(cmd, args);
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
