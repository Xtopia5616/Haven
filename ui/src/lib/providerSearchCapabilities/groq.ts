const SEARCH_MODELS = new Set([
	'openai/gpt-oss-20b',
	'openai/gpt-oss-120b',
	'openai/gpt-oss-safeguard-20b',
]);
const RESPONSES_SEARCH_MODELS = new Set(['openai/gpt-oss-20b', 'openai/gpt-oss-120b']);

export function supportsGroqSearch(
	provider: string,
	baseUrl: string,
	apiStyle: string,
	model: string,
): boolean | undefined {
	if (!provider.includes('groq') && !baseUrl.includes('groq.com')) return undefined;
	if (apiStyle !== 'openai-chat' && apiStyle !== 'openai-responses') return false;
	return (apiStyle === 'openai-chat' ? SEARCH_MODELS : RESPONSES_SEARCH_MODELS).has(
		model.toLowerCase(),
	);
}
