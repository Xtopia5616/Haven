import { supportsDashScopeSearch } from './providerSearchCapabilities/dashscope.ts';
import { supportsDeepSeekSearch } from './providerSearchCapabilities/deepseek.ts';
import { supportsGroqSearch } from './providerSearchCapabilities/groq.ts';
import { supportsOpenAiChatSearch } from './providerSearchCapabilities/openai.ts';
import { supportsOpenRouterSearch } from './providerSearchCapabilities/openrouter.ts';
import { supportsXaiSearch } from './providerSearchCapabilities/xai.ts';
import { normalizeApiStyle, supportsBuiltinWebSearch } from './apiStyleCapabilities.ts';

/** Mirror the model-aware provider checks that gate runtime built-in search. */
export function supportsBuiltinWebSearchForModel(
	style: string | null | undefined,
	provider: string | null | undefined,
	model: string | null | undefined,
	baseUrl: string | null | undefined,
): boolean {
	const apiStyle = normalizeApiStyle(style);
	const vendor = String(provider || '').trim().toLowerCase();
	const modelName = String(model || '').trim();
	const base = String(baseUrl || '').trim().toLowerCase();
	for (const capability of [
		supportsOpenRouterSearch(vendor, base, apiStyle),
		supportsDashScopeSearch(vendor, base, apiStyle, modelName),
		supportsOpenAiChatSearch(vendor, apiStyle, modelName),
		supportsGroqSearch(vendor, base, apiStyle, modelName),
		supportsDeepSeekSearch(vendor, base, apiStyle, modelName),
		supportsXaiSearch(vendor, base, apiStyle, modelName),
	]) {
		if (capability !== undefined) return capability;
	}

	// The shared Responses protocol defines web_search. Chat Completions only
	// supports it on provider/model combinations declared above.
	return apiStyle === 'openai-responses' ||
		(supportsBuiltinWebSearch(apiStyle) && ['anthropic', 'gemini'].includes(apiStyle));
}
