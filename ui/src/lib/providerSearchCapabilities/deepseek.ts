export function supportsDeepSeekSearch(
	provider: string,
	baseUrl: string,
	apiStyle: string,
	model: string,
): boolean | undefined {
	if (!provider.includes('deepseek') && !baseUrl.includes('deepseek.com')) return undefined;
	return apiStyle === 'openai-responses' && model.toLowerCase() === 'deepseek-v4-pro';
}
