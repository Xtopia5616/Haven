export function supportsXaiSearch(
	provider: string,
	baseUrl: string,
	apiStyle: string,
	model: string,
): boolean | undefined {
	if (!provider.includes('xai') && !baseUrl.includes('x.ai')) return undefined;
	if (apiStyle === 'xai') return true;
	return apiStyle === 'openai-responses' && model.toLowerCase().startsWith('grok-4');
}
