import logger from '$lib/logger.ts';
import { normalizeApiStyle, supportsBuiltinWebSearch } from '$lib/apiStyle.ts';
import { invoke } from '$lib/tauri.ts';

type ModelSyncOptions = {
	isDead: () => boolean;
	setModelOptions: (value: any[]) => void;
	setCurrentModelId: (value: string) => void;
	setCurrentModelName: (value: string) => void;
	setCurrentEffort: (value: string) => void;
	setCurrentWebSearch: (value: string) => void;
	setWebSearchSupported: (value: boolean) => void;
	setCurrentApiStyle: (value: string) => void;
};

const defaultModelsCache: {
	baseUrl: string | null;
	list: any[] | null;
	inflight: Promise<any> | null;
	inflightUrl: string | null;
} = {
	baseUrl: null,
	list: null,
	inflight: null,
	inflightUrl: null,
};

/**
 * Owns default-model discovery and settings synchronization for the chat
 * toolbar. The page supplies state setters so this module stays independent
 * of Svelte component state while retaining the module-level discovery cache.
 */
export function createChatModelSync(options: ModelSyncOptions) {
	const {
		isDead,
		setModelOptions,
		setCurrentModelId,
		setCurrentModelName,
		setCurrentEffort,
		setCurrentWebSearch,
		setWebSearchSupported,
		setCurrentApiStyle,
	} = options;

	function ensureDefaultModelOptions(baseUrl: string, providerName: string) {
		if (defaultModelsCache.baseUrl === baseUrl && defaultModelsCache.list) {
			setModelOptions(defaultModelsCache.list);
			return;
		}
		// Settings can swap the default provider while this view stays mounted
		// (keep-alive). Drop the previous endpoint's list; an in-flight fetch
		// for a different URL is abandoned (its .then is stamped and no-ops).
		if (defaultModelsCache.baseUrl !== baseUrl) {
			defaultModelsCache.list = null;
			defaultModelsCache.baseUrl = baseUrl;
		}
		if (defaultModelsCache.inflight && defaultModelsCache.inflightUrl === baseUrl) {
			defaultModelsCache.inflight
				.then((list) => {
					if (!isDead() && defaultModelsCache.baseUrl === baseUrl) setModelOptions(list);
				})
				.catch(() => {
					if (!isDead() && defaultModelsCache.baseUrl === baseUrl) setModelOptions([]);
				});
			return;
		}
		const requestedUrl = baseUrl;
		defaultModelsCache.baseUrl = requestedUrl;
		defaultModelsCache.inflightUrl = requestedUrl;
		defaultModelsCache.inflight = invoke('discover_models', {
			baseUrl: requestedUrl,
			apiKey: '',
			provider: providerName || '',
		})
			.then((list) => {
				const next = list || [];
				// Stale response after a provider swap: ignore.
				if (defaultModelsCache.baseUrl !== requestedUrl) return next;
				defaultModelsCache.list = next;
				if (!isDead()) setModelOptions(next);
				return next;
			})
			.catch((e) => {
				logger.warn('+page', 'discover_models error', e);
				if (!isDead() && defaultModelsCache.baseUrl === requestedUrl) setModelOptions([]);
				throw e;
			})
			.finally(() => {
				// Only clear the coalescing slot when we still own it.
				if (defaultModelsCache.inflightUrl === requestedUrl) {
					defaultModelsCache.inflight = null;
					defaultModelsCache.inflightUrl = null;
				}
			});
		// Swallow the rethrown rejection for the shared in-flight promise;
		// the branch above already surfaces the failure to the UI.
		defaultModelsCache.inflight.catch(() => {});
	}

	/** Apply the default_model role from a get_settings payload to the toolbar. */
	function applyDefaultModelFromSettings(s: any) {
		const dmRole = (/** @type {any[]} */ (s?.llm?.roles || [])).find((r: any) => r.role === 'default_model');
		const dmProvider = dmRole?.provider
			? (/** @type {any[]} */ (s?.llm?.providers || [])).find((p: any) => p.name === dmRole.provider)
			: null;
		const dmModel = dmRole?.model || '';
		setCurrentModelId(dmModel);
		setCurrentModelName(dmModel);
		setCurrentEffort(dmRole?.reasoning_effort || '');
		const webSearch = dmRole?.web_search || 'off';
		setCurrentWebSearch(webSearch);
		const apiStyle = normalizeApiStyle(dmProvider?.api_style || dmProvider?.provider);
		setCurrentApiStyle(apiStyle);
		const webSearchSupported = supportsBuiltinWebSearch(apiStyle);
		setWebSearchSupported(webSearchSupported);
		// Stale auto/always on an unsupported style: clear to off so it cannot
		// resurrect when the user later switches to a supporting provider.
		if (!webSearchSupported && webSearch !== 'off') {
			setCurrentWebSearch('off');
			invoke('set_web_search', { role: 'default_model', mode: 'off' }).catch((e) => {
				logger.warn('+page', 'clear unsupported web_search failed', e);
			});
		} else if (webSearchSupported && apiStyle === 'gemini' && webSearch === 'always') {
			// Gemini Always ≡ Auto; normalize stored value.
			setCurrentWebSearch('auto');
			invoke('set_web_search', { role: 'default_model', mode: 'auto' }).catch((e) => {
				logger.warn('+page', 'normalize gemini web_search always→auto failed', e);
			});
		}
		if (dmProvider?.base_url) {
			ensureDefaultModelOptions(dmProvider.base_url, dmProvider.name);
		} else {
			setModelOptions([]);
		}
	}

	let syncGeneration = 0;

	/** Re-fetch settings and refresh the toolbar default-model controls. */
	function refreshDefaultModelFromBackend() {
		const generation = ++syncGeneration;
		invoke('get_settings')
			.then((s) => {
				if (isDead() || generation !== syncGeneration) return;
				applyDefaultModelFromSettings(s);
			})
			.catch((e) => {
				logger.warn('+page', 'refresh default model error', e);
			});
	}

	return { applyDefaultModelFromSettings, refreshDefaultModelFromBackend };
}
