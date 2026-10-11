import { isRecord } from './contracts/objectGuards.ts';

/** Reject model drafts that Settings would otherwise discard or route ambiguously. */
export function validateSettingsModelDrafts(models: unknown, providers: unknown): void {
	if (!Array.isArray(models)) throw new Error('模型配置列表无效，请重新加载设置。');
	const configuredProviderNames = new Set(
		(Array.isArray(providers) ? providers : [])
			.filter(isRecord)
			.map((provider) => provider.name)
			.filter((name): name is string => typeof name === 'string'),
	);
	const modelIds = new Set<string>();

	for (const [index, candidate] of models.entries()) {
		if (!isRecord(candidate)) {
			throw new Error(`第 ${index + 1} 个模型配置无效，请重新创建该模型。`);
		}
		const modelId = typeof candidate.id === 'string' ? candidate.id.trim() : '';
		if (!modelId) {
			throw new Error(`第 ${index + 1} 个模型缺少配置 ID，请重新创建该模型。`);
		}
		if (modelIds.has(modelId)) {
			throw new Error(`模型配置 ID「${modelId}」重复；每个模型都必须有唯一 ID。`);
		}
		modelIds.add(modelId);
		if (typeof candidate.model !== 'string' || !candidate.model.trim()) {
			throw new Error(`模型「${modelId}」尚未选择 Provider 服务模型。`);
		}
		if (
			typeof candidate.providerName !== 'string' ||
			!configuredProviderNames.has(candidate.providerName)
		) {
			throw new Error(`模型「${modelId}」必须绑定已配置的 Provider。`);
		}
	}
}
