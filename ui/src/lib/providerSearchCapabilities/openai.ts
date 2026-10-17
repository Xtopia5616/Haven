export function supportsOpenAiChatSearch(
	provider: string,
	apiStyle: string,
	model: string,
): boolean | undefined {
	if (provider !== 'openai') return undefined;
	if (apiStyle !== 'openai-chat') return undefined;
	return model.toLowerCase() === 'gpt-5-search-api';
}
