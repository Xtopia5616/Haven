import logger from '$lib/logger.ts';
import { reportError } from '$lib/errorHandling.ts';
import { formatError } from '$lib/formatError.ts';
import { addNotification } from '$lib/stores.ts';
import { invoke } from '$lib/tauri.ts';

type Provider = {
	name: string;
	base_url: string;
	api_key?: string;
	[key: string]: unknown;
};

type Model = {
	id: string;
	name?: string;
	context_window?: number;
	cost_per_1k_input_tokens?: number;
	cost_per_1k_output_tokens?: number;
	[key: string]: unknown;
};

type ModelMap = Record<string, Model[]>;

export interface ModelDiscoveryContext {
	getProviders: () => Provider[];
	getRoles: () => Array<Record<string, any>>;
	getModels: () => ModelMap;
	setModels: (models: ModelMap) => void;
	isProviderFetching?: (providerName: string) => boolean;
	isRefreshingAll?: () => boolean;
	setProviderFetching: (providerName: string, fetching: boolean) => void;
	setRefreshingAll: (refreshing: boolean) => void;
	onDiscoverySettled?: (fills: Array<Record<string, unknown>>) => void;
}

/**
 * Apply metadata without replacing values explicitly entered by the user.
 * Selecting a model uses overwrite=true, while a background refresh only
 * fills empty fields.
 */
export function applyDiscoveredModelMeta(
	slot: Record<string, any>,
	models: ModelMap,
	providerName: string,
	modelId: string,
	{ overwrite = false }: { overwrite?: boolean } = {},
): Record<string, unknown> {
	const model = (models[providerName] || []).find((item) => item.id === modelId);
	if (!model) {
		if (overwrite) {
			slot.context_window = null;
			slot.cost_per_1k_input_tokens = null;
			slot.cost_per_1k_output_tokens = null;
			slot.cost_per_1k_cache_read_tokens = null;
			slot.cost_per_1k_cache_write_tokens = null;
		}
		return {};
	}
	const wrote: Record<string, unknown> = {};
	if (overwrite || slot.context_window == null || slot.context_window === 0) {
		const next = model.context_window && model.context_window > 0 ? model.context_window : null;
		if (slot.context_window !== next) {
			slot.context_window = next;
			wrote.context_window = next;
		}
	}
	if (overwrite || slot.cost_per_1k_input_tokens == null) {
		const next =
			typeof model.cost_per_1k_input_tokens === 'number'
				? model.cost_per_1k_input_tokens
				: null;
		if (slot.cost_per_1k_input_tokens !== next) {
			slot.cost_per_1k_input_tokens = next;
			wrote.cost_per_1k_input_tokens = next;
		}
	}
	if (overwrite || slot.cost_per_1k_output_tokens == null) {
		const next =
			typeof model.cost_per_1k_output_tokens === 'number'
				? model.cost_per_1k_output_tokens
				: null;
		if (slot.cost_per_1k_output_tokens !== next) {
			slot.cost_per_1k_output_tokens = next;
			wrote.cost_per_1k_output_tokens = next;
		}
	}
	return wrote;
}

/** Keep provider / role mutation in the settings owner, not in discovery. */
export function createModelDiscovery(context: ModelDiscoveryContext) {
	let lastRefreshNotify = 0;

	function backfillRoleMetaFromDiscovery() {
		const fills: Array<Record<string, unknown>> = [];
		for (const slot of context.getRoles()) {
			if (!slot?.provider || !slot?.model) continue;
			const wrote = applyDiscoveredModelMeta(
				slot,
				context.getModels(),
				slot.provider,
				slot.model,
			);
			if (Object.keys(wrote).length) fills.push({ role: slot.role, ...wrote });
		}
		context.onDiscoverySettled?.(fills);
	}

	async function refreshProviderModels(providerName: string): Promise<boolean> {
		const provider = context.getProviders().find((item) => item.name === providerName);
		if (!provider || !provider.base_url.trim()) return false;
		if (context.isProviderFetching?.(providerName)) return false;
		context.setProviderFetching(providerName, true);
		try {
			const list = await invoke('discover_models', {
				baseUrl: provider.base_url,
				apiKey: provider.api_key || '',
				provider: providerName,
			});
			context.setModels({ ...context.getModels(), [providerName]: list || [] });
			backfillRoleMetaFromDiscovery();
		} catch (error) {
			logger.warn(
				'modelDiscovery',
				`discover_models ${providerName} error`,
				formatError(error),
			);
			return false;
		} finally {
			context.setProviderFetching(providerName, false);
		}
		return true;
	}

	async function refreshAllModels(silent = false) {
		if (context.isRefreshingAll?.()) return;
		const providers = context.getProviders().filter((provider) => provider.base_url.trim());
		if (!providers.length) return;
		context.setRefreshingAll(true);
		try {
			let failedProviders: string[] = [];
			if (providers.some((provider) => provider.api_key)) {
				const results = await Promise.all(
					providers.map(async (provider) => ({
						name: provider.name,
						ok: await refreshProviderModels(provider.name),
					})),
				);
				failedProviders = results
					.filter((result) => !result.ok)
					.map((result) => result.name);
			} else {
				context.setModels((await invoke('discover_all_models')) || {});
			}
			backfillRoleMetaFromDiscovery();
			if (!silent && Date.now() - lastRefreshNotify > 2500) {
				lastRefreshNotify = Date.now();
				if (failedProviders.length) {
					const label = failedProviders.join('、');
					addNotification(
						failedProviders.length === providers.length
							? `模型列表刷新失败：${label}`
							: `部分模型提供商刷新失败：${label}`,
						failedProviders.length === providers.length ? 'error' : 'warning',
						4000,
					);
				} else {
					addNotification('模型列表已刷新', 'success', 2500);
				}
			}
		} catch (error) {
			reportError(error, {
				context: 'modelDiscovery',
				message: '刷新模型列表失败',
				log: false,
			});
		} finally {
			context.setRefreshingAll(false);
		}
	}

	return {
		backfillRoleMetaFromDiscovery,
		refreshAllModels,
		refreshProviderModels,
		applyDiscoveredModelMeta: (
			slot: Record<string, any>,
			providerName: string,
			modelId: string,
			options?: { overwrite?: boolean },
		) => applyDiscoveredModelMeta(slot, context.getModels(), providerName, modelId, options),
		modelOptions: (providerName: string) =>
			(context.getModels()[providerName] || []).map((model) => ({
				value: model.id,
				label: model.name || model.id,
			})),
	};
}
