const CHAT_MODEL_PREFIXES = [
	'qwen3.5',
	'qwen3.6',
	'qwen3.7',
	'qwen3.8',
	'deepseek-v4',
	'qwen-max',
	'qwen-plus',
	'qwen-flash',
];

const RESPONSES_MODEL_PREFIXES = [
	'qwen3.5-plus',
	'qwen3.5-flash',
	'qwen3.5-27b',
	'qwen3.5-35b-a3b',
	'qwen3.5-122b-a10b',
	'qwen3.5-397b-a17b',
	'qwen3.6-plus',
	'qwen3.6-flash',
	'qwen3.6-35b-a3b',
	'qwen3.7-max',
	'qwen3.7-plus',
	'qwen3.7-flash',
	'qwen3.8-max',
	'qwen3.8-flash',
	'qwen3.8-2.4t-a95b',
	'qwen3.8-27b',
	'qwen3.8-omni-flash',
	'deepseek-v4-pro',
	'deepseek-v4-flash-0731',
	'glm-5.2',
	'kimi-k3',
];

function startsWithAny(model: string, prefixes: string[]): boolean {
	const normalized = model.toLowerCase();
	return prefixes.some((prefix) => normalized.startsWith(prefix));
}

export function supportsDashScopeSearch(
	provider: string,
	baseUrl: string,
	apiStyle: string,
	model: string,
): boolean | undefined {
	const vendor = provider.includes('dashscope') || /dashscope|\.maas\.aliyuncs\.com/.test(baseUrl);
	if (!vendor) return undefined;
	if (apiStyle === 'dashscope') return startsWithAny(model, CHAT_MODEL_PREFIXES);
	if (apiStyle === 'openai-responses') return startsWithAny(model, RESPONSES_MODEL_PREFIXES);
	return false;
}
