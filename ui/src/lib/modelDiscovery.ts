import { reportError } from '$lib/errorHandling.ts';
import { addNotification } from '$lib/notificationStore.ts';
import { isKeylessProvider } from '$lib/apiStyle.ts';
import { discoverModels } from '$lib/modelDiscoveryCommands.ts';
import type { DiscoveredModelsByProviderName, ModelInfo } from '$lib/contracts/model.ts';
import type { ModelDraft, ProviderDraft } from '$lib/settingsModelTypes.ts';

export type ModelDiscoveryProvider = Pick<
	ProviderDraft,
	| 'name'
	| 'base_url'
	| 'api_key'
	| 'api_key_ref'
	| 'api_style'
	| 'provider'
	| 'auth_header_name'
	| 'auth_header_prefix'
	| 'proxy_url'
	| 'no_proxy'
>;

type DiscoveredModelMetadataPatch = Partial<
	Pick<ModelDraft, 'context_window' | 'cost_per_1k_input_tokens' | 'cost_per_1k_output_tokens'>
>;

type ModelDiscoverySkipReason =
	| 'provider_not_found'
	| 'missing_base_url'
	| 'missing_credentials'
	| 'already_in_progress'
	| 'configuration_changed';

type ModelDiscoveryRefreshOutcome =
	| { status: 'discovered'; models: ModelInfo[] }
	| { status: 'failed' }
	| { status: 'skipped'; reason: ModelDiscoverySkipReason };

export type DiscoveredModelMetadataFill = Pick<ModelDraft, 'id'> & DiscoveredModelMetadataPatch;

export interface ModelDiscoveryContext {
	getProviders: () => ModelDiscoveryProvider[];
	getModels: () => ModelDraft[];
	getDiscoveredModels: () => DiscoveredModelsByProviderName;
	setModels: (models: DiscoveredModelsByProviderName) => void;
	isProviderConfigured: (provider: ModelDiscoveryProvider) => boolean;
	isProviderFetching?: (providerName: string) => boolean;
	isRefreshingAll?: () => boolean;
	setProviderFetching: (providerName: string, fetching: boolean) => void;
	setRefreshingAll: (refreshing: boolean) => void;
	onDiscoverySettled?: (fills: DiscoveredModelMetadataFill[]) => void;
}

/**
 * Apply metadata without replacing values explicitly entered by the user.
 * Selecting a model uses overwrite=true, while a background refresh only
 * fills empty fields.
 */
export function applyDiscoveredModelMeta(
	slot: ModelDraft,
	models: DiscoveredModelsByProviderName,
	providerName: string,
	modelId: string,
	{ overwrite = false }: { overwrite?: boolean } = {},
): DiscoveredModelMetadataPatch {
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
	const wrote: DiscoveredModelMetadataPatch = {};
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

/** Keep provider / model mutation in the settings owner, not in discovery. */
export function createModelDiscovery(context: ModelDiscoveryContext) {
	let lastRefreshNotify = 0;

	function clearProviderCatalog(providerName: string) {
		const current = context.getDiscoveredModels();
		if (!(providerName in current)) return;
		const next = { ...current };
		delete next[providerName];
		context.setModels(next);
	}

	function pruneProviderCatalogs(providers: ModelDiscoveryProvider[]) {
		const configuredNames = new Set(
			providers
				.filter(
					(provider) =>
						provider.base_url.trim() && context.isProviderConfigured(provider),
				)
				.map((provider) => provider.name),
		);
		const next = { ...context.getDiscoveredModels() };
		let changed = false;
		for (const name of Object.keys(next)) {
			if (!configuredNames.has(name)) {
				delete next[name];
				changed = true;
			}
		}
		if (changed) context.setModels(next);
	}

	function backfillModelMetaFromDiscovery() {
		const fills: DiscoveredModelMetadataFill[] = [];
		for (const slot of context.getModels()) {
			if (!slot?.providerName || !slot?.model) continue;
			const wrote = applyDiscoveredModelMeta(
				slot,
				context.getDiscoveredModels(),
				slot.providerName,
				slot.model,
			);
			if (Object.keys(wrote).length) fills.push({ id: slot.id, ...wrote });
		}
		context.onDiscoverySettled?.(fills);
	}

	async function refreshProviderModels(
		providerName: string,
		{
			notifyOnError = true,
			backfill = true,
		}: { notifyOnError?: boolean; backfill?: boolean } = {},
	): Promise<ModelDiscoveryRefreshOutcome> {
		return refreshProviderModelsInternal(providerName, { notifyOnError, backfill }, true);
	}

	async function retryChangedDiscoveryTarget(
		providerName: string,
		requestedProvider: ModelDiscoveryProvider,
		options: { notifyOnError: boolean; backfill: boolean },
		retryAfterConfigurationChange: boolean,
	): Promise<ModelDiscoveryRefreshOutcome | null> {
		const currentProvider = context.getProviders().find((item) => item.name === providerName);
		if (currentProvider && isSameModelDiscoveryTarget(requestedProvider, currentProvider)) {
			return null;
		}

		clearProviderCatalog(providerName);
		if (
			retryAfterConfigurationChange &&
			currentProvider?.base_url.trim() &&
			context.isProviderConfigured(currentProvider)
		) {
			context.setProviderFetching(providerName, false);
			return refreshProviderModelsInternal(providerName, options, false);
		}
		return skipped('configuration_changed');
	}

	async function refreshProviderModelsInternal(
		providerName: string,
		{ notifyOnError = true, backfill = true }: { notifyOnError?: boolean; backfill?: boolean },
		retryAfterConfigurationChange: boolean,
	): Promise<ModelDiscoveryRefreshOutcome> {
		const provider = context.getProviders().find((item) => item.name === providerName);
		if (!provider) {
			clearProviderCatalog(providerName);
			return skipped('provider_not_found');
		}
		if (!provider.base_url.trim()) {
			clearProviderCatalog(providerName);
			return skipped('missing_base_url');
		}
		if (!context.isProviderConfigured(provider)) {
			clearProviderCatalog(providerName);
			if (notifyOnError) {
				addNotification(`请先为 ${providerName} 配置可用凭据`, 'warning', 3500);
			}
			return skipped('missing_credentials');
		}
		if (context.isProviderFetching?.(providerName)) return skipped('already_in_progress');
		context.setProviderFetching(providerName, true);
		try {
			const hasCustomAuthScheme =
				provider.auth_header_name !== 'Authorization' ||
				provider.auth_header_prefix !== 'Bearer';
			const list = await discoverModels({
				baseUrl: provider.base_url,
				apiKey: provider.api_key || '',
				providerName,
				...(hasCustomAuthScheme
					? {
							authHeaderName: provider.auth_header_name,
							authHeaderPrefix: provider.auth_header_prefix,
						}
					: {}),
				proxyUrl: provider.proxy_url ?? null,
				noProxy: provider.no_proxy ?? null,
				...(isKeylessProvider(provider) ? { skipAuth: true } : {}),
			});
			const targetChangeOutcome = await retryChangedDiscoveryTarget(
				providerName,
				provider,
				{ notifyOnError, backfill },
				retryAfterConfigurationChange,
			);
			if (targetChangeOutcome) return targetChangeOutcome;
			const outcome: ModelDiscoveryRefreshOutcome = { status: 'discovered', models: list };
			context.setModels({ ...context.getDiscoveredModels(), [providerName]: list });
			if (backfill) backfillModelMetaFromDiscovery();
			return outcome;
		} catch (error) {
			const targetChangeOutcome = await retryChangedDiscoveryTarget(
				providerName,
				provider,
				{ notifyOnError, backfill },
				retryAfterConfigurationChange,
			);
			if (targetChangeOutcome) return targetChangeOutcome;
			// Bulk refreshes own one aggregate toast; individual provider refreshes
			// report their own error unless the caller has another visible status.
			reportError(error, {
				context: 'modelDiscovery',
				message: `获取 ${providerName} 模型列表失败`,
				notify: notifyOnError,
			});
			return { status: 'failed' };
		} finally {
			context.setProviderFetching(providerName, false);
		}
	}

	async function refreshAllModels(silent = false) {
		if (context.isRefreshingAll?.()) return;
		const allProviders = context.getProviders();
		if (!allProviders.length) return;
		pruneProviderCatalogs(allProviders);
		const candidates = allProviders.filter((provider) => provider.base_url.trim());
		if (!candidates.length) {
			if (!silent) addNotification('请先为 Provider 配置模型服务地址', 'info', 3000);
			return;
		}
		const providers = candidates.filter(context.isProviderConfigured);
		if (!providers.length) {
			if (!silent) {
				addNotification('没有已配置凭据且可查询的 Provider', 'info', 3000);
			}
			return;
		}
		context.setRefreshingAll(true);
		try {
			const failedProviders: string[] = [];
			const skippedProviders = candidates
				.filter((provider) => !context.isProviderConfigured(provider))
				.map((provider) => provider.name);
			let discoveredCount = 0;
			const results = await Promise.all(
				providers.map(async (provider) => ({
					name: provider.name,
					outcome: await refreshProviderModels(provider.name, {
						notifyOnError: false,
						backfill: false,
					}),
				})),
			);
			for (const result of results) {
				switch (result.outcome.status) {
					case 'discovered':
						discoveredCount += 1;
						break;
					case 'failed':
						failedProviders.push(result.name);
						break;
					case 'skipped':
						skippedProviders.push(result.name);
						break;
				}
			}
			backfillModelMetaFromDiscovery();
			if (!silent && Date.now() - lastRefreshNotify > 2500) {
				lastRefreshNotify = Date.now();
				if (failedProviders.length) {
					const label = failedProviders.join('、');
					const allAttemptedProvidersFailed = discoveredCount === 0;
					addNotification(
						allAttemptedProvidersFailed
							? `模型列表刷新失败：${label}`
							: `部分模型提供商刷新失败：${label}`,
						allAttemptedProvidersFailed ? 'error' : 'warning',
						4000,
					);
				} else if (!discoveredCount) {
					addNotification('没有可刷新的已配置 Provider', 'info', 3000);
				} else if (skippedProviders.length) {
					addNotification(
						`已刷新 ${discoveredCount} 个 Provider，另有 ${skippedProviders.length} 个未刷新`,
						'info',
						3000,
					);
				} else {
					addNotification('模型列表已刷新', 'success', 2500);
				}
			}
		} catch (error) {
			reportError(error, {
				context: 'modelDiscovery',
				message: '刷新模型列表失败',
				notify: !silent,
			});
		} finally {
			context.setRefreshingAll(false);
		}
	}

	return {
		backfillModelMetaFromDiscovery,
		refreshAllModels,
		refreshProviderModels,
		applyDiscoveredModelMeta: (
			slot: ModelDraft,
			providerName: string,
			modelId: string,
			options?: { overwrite?: boolean },
		) =>
			applyDiscoveredModelMeta(
				slot,
				context.getDiscoveredModels(),
				providerName,
				modelId,
				options,
			),
		invalidateProviderCatalog: clearProviderCatalog,
		modelOptions: (providerName: string) =>
			(context.getDiscoveredModels()[providerName] || []).map((model) => ({
				value: model.id,
				label: model.name || model.id,
			})),
	};
}

function skipped(reason: ModelDiscoverySkipReason): ModelDiscoveryRefreshOutcome {
	return { status: 'skipped', reason };
}

export function isSameModelDiscoveryTarget(
	a: ModelDiscoveryProvider,
	b: ModelDiscoveryProvider,
): boolean {
	return (
		a.name === b.name &&
		a.provider === b.provider &&
		a.api_style === b.api_style &&
		a.base_url === b.base_url &&
		a.api_key === b.api_key &&
		a.api_key_ref === b.api_key_ref &&
		a.auth_header_name === b.auth_header_name &&
		a.auth_header_prefix === b.auth_header_prefix &&
		a.proxy_url === b.proxy_url &&
		a.no_proxy === b.no_proxy
	);
}
