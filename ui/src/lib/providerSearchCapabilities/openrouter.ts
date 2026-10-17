export function supportsOpenRouterSearch(
	provider: string,
	baseUrl: string,
	apiStyle: string,
): boolean | undefined {
	if (!provider.includes('openrouter') && !baseUrl.includes('openrouter.ai')) return undefined;
	return apiStyle === 'openrouter' || apiStyle === 'openai-responses';
}
