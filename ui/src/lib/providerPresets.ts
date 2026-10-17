/** Provider identity presets, editor transitions, and keyless profiles. */
import {
	isOpenaiFamilyWireStyle,
	defaultProviderMediaCapabilities,
	normalizeApiStyle,
	providerWireStyle,
} from './apiStyleCapabilities.ts';
import type {
	ProviderMediaCapabilitySettings,
	ProviderStyleInput,
} from './apiStyleCapabilities.ts';

export interface ProviderPreset {
	value: string;
	label: string;
	group: string;
	api_style: string;
	provider: string;
	base_url: string;
	auth_header_name: string;
	auth_header_prefix: string;
	media_capabilities?: ProviderMediaCapabilitySettings;
	docs_url?: string;
	console_url?: string;
	hint?: string;
	match_hosts?: string[];
	keyless?: boolean;
}

const BEARER = /** @type {const} */ {
	auth_header_name: 'Authorization',
	auth_header_prefix: 'Bearer',
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
	// —— 原厂服务 ——
	{
		value: 'openai-chat',
		label: 'OpenAI Chat Completions',
		group: '原厂服务',
		api_style: 'openai-chat',
		provider: 'openai',
		base_url: 'https://api.openai.com/v1',
		...BEARER,
		docs_url: 'https://platform.openai.com/docs/api-reference',
		console_url: 'https://platform.openai.com/api-keys',
		hint: '标准 Chat Completions；OpenAI 的 gpt-5-search-api 可通过此接口使用联网搜索。',
		match_hosts: ['api.openai.com'],
	},
	{
		value: 'openai-responses',
		label: 'OpenAI Responses',
		group: '原厂服务',
		api_style: 'openai-responses',
		provider: 'openai',
		base_url: 'https://api.openai.com/v1',
		...BEARER,
		docs_url: 'https://platform.openai.com/docs/api-reference/responses',
		console_url: 'https://platform.openai.com/api-keys',
		hint: 'Responses API；支持内置联网搜索与 reasoning。',
		match_hosts: ['api.openai.com'],
	},
	{
		value: 'openai-azure',
		label: 'Azure OpenAI',
		group: '原厂服务',
		api_style: 'openai-chat',
		provider: 'azure',
		base_url: 'https://YOUR_RESOURCE.openai.azure.com/openai',
		auth_header_name: 'api-key',
		auth_header_prefix: '',
		docs_url: 'https://learn.microsoft.com/azure/ai-services/openai/',
		console_url: 'https://portal.azure.com/',
		hint: '把 YOUR_RESOURCE 换成资源名；部署名填到角色模型。模型列表可能需 api-version 查询参数。',
		match_hosts: ['openai.azure.com'],
	},
	{
		value: 'anthropic',
		label: 'Anthropic (Claude)',
		group: '原厂服务',
		api_style: 'anthropic',
		provider: 'anthropic',
		base_url: 'https://api.anthropic.com',
		auth_header_name: 'x-api-key',
		auth_header_prefix: '',
		docs_url: 'https://docs.anthropic.com/en/api/getting-started',
		console_url: 'https://console.anthropic.com/',
		hint: 'Messages API；支持 server web_search 与 extended thinking。',
		match_hosts: ['api.anthropic.com'],
	},
	{
		value: 'gemini',
		label: 'Google Gemini',
		group: '原厂服务',
		api_style: 'gemini',
		provider: 'gemini',
		base_url: 'https://generativelanguage.googleapis.com/v1beta',
		auth_header_name: 'x-goog-api-key',
		auth_header_prefix: '',
		docs_url: 'https://ai.google.dev/gemini-api/docs',
		console_url: 'https://aistudio.google.com/apikey',
		hint: 'generateContent；可选 Google Search grounding；也可作生图后端。',
		match_hosts: ['generativelanguage.googleapis.com'],
	},
	{
		value: 'xai',
		label: 'xAI Grok (Web Search)',
		group: '原厂服务',
		api_style: 'openai-responses',
		provider: 'xai',
		base_url: 'https://api.x.ai/v1',
		...BEARER,
		docs_url: 'https://docs.x.ai/developers/tools/web-search',
		console_url: 'https://console.x.ai/',
		hint: 'Responses API + 服务端 web_search；旧 Chat Completions 配置仍可用。',
		match_hosts: ['api.x.ai'],
	},
	{
		value: 'mistral',
		label: 'Mistral AI',
		group: '原厂服务',
		api_style: 'openai-chat',
		provider: 'mistral',
		base_url: 'https://api.mistral.ai/v1',
		...BEARER,
		docs_url: 'https://docs.mistral.ai/',
		console_url: 'https://console.mistral.ai/',
		hint: 'OpenAI 兼容 Chat Completions。',
		match_hosts: ['api.mistral.ai'],
	},

	// —— 模型聚合与推理托管 ——
	{
		value: 'openrouter',
		label: 'OpenRouter',
		group: '模型聚合',
		api_style: 'openrouter',
		provider: 'openrouter',
		base_url: 'https://openrouter.ai/api/v1',
		...BEARER,
		docs_url: 'https://openrouter.ai/docs',
		console_url: 'https://openrouter.ai/keys',
		hint: '多模型聚合；新 server tool 为 openrouter:web_search，旧 :online 已弃用。',
		match_hosts: ['openrouter.ai'],
	},
	{
		value: 'groq',
		label: 'Groq',
		group: '推理托管',
		api_style: 'openai-responses',
		provider: 'groq',
		base_url: 'https://api.groq.com/openai/v1',
		...BEARER,
		docs_url: 'https://console.groq.com/docs/responses-api',
		console_url: 'https://console.groq.com/keys',
		hint: 'Responses API；GPT-OSS 20B/120B 支持 browser_search；也可作 Whisper STT。',
		match_hosts: ['api.groq.com'],
	},
	{
		value: 'groq-chat',
		label: 'Groq Chat Completions',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'groq',
		base_url: 'https://api.groq.com/openai/v1',
		...BEARER,
		docs_url: 'https://console.groq.com/docs/tool-use/built-in-tools/browser-search',
		console_url: 'https://console.groq.com/keys',
		hint: 'GPT-OSS 20B/120B/Safeguard 20B 支持 browser_search；不兼容结构化输出。',
		match_hosts: ['api.groq.com'],
	},
	{
		value: 'together',
		label: 'Together AI',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'together',
		base_url: 'https://api.together.xyz/v1',
		...BEARER,
		docs_url: 'https://docs.together.ai/',
		console_url: 'https://api.together.xyz/',
		hint: 'OpenAI 兼容开源模型托管。',
		match_hosts: ['api.together.xyz'],
	},
	{
		value: 'fireworks',
		label: 'Fireworks AI',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'fireworks',
		base_url: 'https://api.fireworks.ai/inference/v1',
		...BEARER,
		docs_url: 'https://docs.fireworks.ai/',
		console_url: 'https://fireworks.ai/account/api-keys',
		hint: 'OpenAI 兼容推理 API。',
		match_hosts: ['api.fireworks.ai'],
	},
	{
		value: 'deepinfra',
		label: 'DeepInfra',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'deepinfra',
		base_url: 'https://api.deepinfra.com/v1/openai',
		...BEARER,
		docs_url: 'https://deepinfra.com/docs',
		console_url: 'https://deepinfra.com/dash/api_keys',
		hint: 'OpenAI 兼容端点（/v1/openai）。',
		match_hosts: ['api.deepinfra.com'],
	},
	{
		value: 'cerebras',
		label: 'Cerebras',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'cerebras',
		base_url: 'https://api.cerebras.ai/v1',
		...BEARER,
		docs_url: 'https://inference-docs.cerebras.ai/',
		console_url: 'https://cloud.cerebras.ai/',
		hint: 'OpenAI 兼容高速推理。',
		match_hosts: ['api.cerebras.ai'],
	},
	{
		value: 'nvidia',
		label: 'NVIDIA NIM',
		group: '推理托管',
		api_style: 'openai-chat',
		provider: 'nvidia',
		base_url: 'https://integrate.api.nvidia.com/v1',
		...BEARER,
		docs_url: 'https://docs.api.nvidia.com/',
		console_url: 'https://build.nvidia.com/',
		hint: 'NVIDIA API Catalog / NIM OpenAI 兼容端点。',
		match_hosts: ['integrate.api.nvidia.com'],
	},
	{
		value: 'perplexity',
		label: 'Perplexity',
		group: '专用 API',
		api_style: 'openai-chat',
		provider: 'perplexity',
		base_url: 'https://api.perplexity.ai',
		...BEARER,
		docs_url: 'https://docs.perplexity.ai/',
		console_url: 'https://www.perplexity.ai/settings/api',
		hint: 'Chat Completions；模型列表接口可能不可用，需手填模型名。',
		match_hosts: ['api.perplexity.ai'],
	},
	{
		value: 'cohere',
		label: 'Cohere (OpenAI compat)',
		group: '专用 API',
		api_style: 'openai-chat',
		provider: 'cohere',
		base_url: 'https://api.cohere.ai/compatibility/v1',
		...BEARER,
		docs_url: 'https://docs.cohere.com/docs/compatibility-api',
		console_url: 'https://dashboard.cohere.com/api-keys',
		hint: '走 Cohere OpenAI Compatibility API。',
		match_hosts: ['api.cohere.ai'],
	},

	// —— 国内服务 ——
	{
		value: 'deepseek-chat',
		label: 'DeepSeek Chat',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'deepseek',
		base_url: 'https://api.deepseek.com',
		...BEARER,
		docs_url: 'https://api-docs.deepseek.com/',
		console_url: 'https://platform.deepseek.com/api_keys',
		hint: 'Chat Completions；思考模型可用聊天页「思考强度」。',
		match_hosts: ['api.deepseek.com'],
	},
	{
		value: 'deepseek-responses',
		label: 'DeepSeek Responses（思考+联网）',
		group: '国内服务',
		api_style: 'openai-responses',
		provider: 'deepseek',
		base_url: 'https://api.deepseek.com',
		...BEARER,
		docs_url: 'https://api-docs.deepseek.com/guides/reasoning_model',
		console_url: 'https://platform.deepseek.com/api_keys',
		hint: 'Responses 协议；思考内容回显 + 内置 web_search。',
		match_hosts: ['api.deepseek.com'],
	},
	{
		value: 'moonshot',
		label: 'Moonshot / Kimi',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'moonshot',
		base_url: 'https://api.moonshot.cn/v1',
		...BEARER,
		docs_url: 'https://platform.moonshot.cn/docs',
		console_url: 'https://platform.moonshot.cn/console/api-keys',
		hint: 'Kimi；thinking 扩展由 adapter 按厂商检测启用。',
		match_hosts: ['api.moonshot.cn', 'api.moonshot.ai'],
	},
	{
		value: 'dashscope',
		label: '通义千问 (DashScope)',
		group: '国内服务',
		api_style: 'dashscope',
		provider: 'dashscope',
		base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
		...BEARER,
		docs_url: 'https://help.aliyun.com/zh/model-studio/',
		console_url: 'https://dashscope.console.aliyun.com/',
		hint: 'Chat enable_search/forced_search 并可读用量；Responses web_search 返回来源。',
		match_hosts: ['dashscope.aliyuncs.com', 'dashscope-intl.aliyuncs.com'],
	},
	{
		value: 'dashscope-responses',
		label: '通义千问 (DashScope Responses)',
		group: '国内服务',
		api_style: 'openai-responses',
		provider: 'dashscope',
		base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
		...BEARER,
		docs_url: 'https://help.aliyun.com/en/model-studio/web-search',
		console_url: 'https://dashscope.console.aliyun.com/',
		hint: 'Responses web_search；仅官方列出的 Qwen、DeepSeek、GLM、Kimi 模型支持，是否搜索由模型决定。',
		match_hosts: ['dashscope.aliyuncs.com', 'dashscope-intl.aliyuncs.com'],
	},
	{
		value: 'zhipu',
		label: '智谱 GLM',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'zhipu',
		base_url: 'https://open.bigmodel.cn/api/paas/v4',
		...BEARER,
		docs_url: 'https://docs.bigmodel.cn/',
		console_url: 'https://open.bigmodel.cn/usercenter/apikeys',
		hint: '智谱开放平台 OpenAI 兼容接口。',
		match_hosts: ['open.bigmodel.cn'],
	},
	{
		value: 'siliconflow',
		label: '硅基流动 SiliconFlow',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'siliconflow',
		base_url: 'https://api.siliconflow.cn/v1',
		...BEARER,
		docs_url: 'https://docs.siliconflow.cn/',
		console_url: 'https://cloud.siliconflow.cn/account/ak',
		hint: '国内多模型聚合，OpenAI 兼容。',
		match_hosts: ['api.siliconflow.cn'],
	},
	{
		value: 'doubao',
		label: '豆包 / 火山方舟',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'doubao',
		base_url: 'https://ark.cn-beijing.volces.com/api/v3',
		...BEARER,
		docs_url: 'https://www.volcengine.com/docs/82379',
		console_url: 'https://console.volcengine.com/ark',
		hint: '模型名填接入点/Endpoint ID（ep-…），不是展示名。',
		match_hosts: ['ark.cn-beijing.volces.com', 'volces.com'],
	},
	{
		value: 'minimax',
		label: 'MiniMax',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'minimax',
		base_url: 'https://api.minimax.chat/v1',
		...BEARER,
		docs_url: 'https://platform.minimaxi.com/document/',
		console_url: 'https://platform.minimaxi.com/user-center/basic-information/interface-key',
		hint: 'OpenAI 兼容；部分能力需 GroupId 等扩展字段（后续可加）。',
		match_hosts: ['api.minimax.chat', 'api.minimaxi.com'],
	},
	{
		value: 'stepfun',
		label: '阶跃星辰 StepFun',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'stepfun',
		base_url: 'https://api.stepfun.com/v1',
		...BEARER,
		docs_url: 'https://platform.stepfun.com/docs',
		console_url: 'https://platform.stepfun.com/',
		hint: 'OpenAI 兼容。',
		match_hosts: ['api.stepfun.com'],
	},
	{
		value: 'baichuan',
		label: '百川 Baichuan',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'baichuan',
		base_url: 'https://api.baichuan-ai.com/v1',
		...BEARER,
		docs_url: 'https://platform.baichuan-ai.com/docs',
		console_url: 'https://platform.baichuan-ai.com/',
		hint: 'OpenAI 兼容。',
		match_hosts: ['api.baichuan-ai.com'],
	},
	{
		value: 'yi',
		label: '零一万物 Yi',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'yi',
		base_url: 'https://api.lingyiwanwu.com/v1',
		...BEARER,
		docs_url: 'https://platform.lingyiwanwu.com/docs',
		console_url: 'https://platform.lingyiwanwu.com/',
		hint: 'OpenAI 兼容。',
		match_hosts: ['api.lingyiwanwu.com'],
	},
	{
		value: 'hunyuan',
		label: '腾讯混元',
		group: '国内服务',
		api_style: 'openai-chat',
		provider: 'hunyuan',
		base_url: 'https://api.hunyuan.cloud.tencent.com/v1',
		...BEARER,
		docs_url: 'https://cloud.tencent.com/document/product/1729',
		console_url: 'https://console.cloud.tencent.com/hunyuan',
		hint: 'OpenAI 兼容；密钥在混元控制台创建。',
		match_hosts: ['hunyuan.cloud.tencent.com'],
	},

	{
		value: 'openai-compatible',
		label: '自定义 OpenAI-compatible',
		group: '自定义兼容服务',
		api_style: 'openai-chat',
		provider: 'openai-compatible',
		base_url: 'https://api.example.com/v1',
		...BEARER,
		hint: '仅预设 Chat Completions 协议。STT、TTS 和生图端点默认关闭，可按服务实际支持情况单独启用。',
	},

	// —— 本地服务 ——
	{
		value: 'llama.cpp',
		label: 'llama.cpp server',
		group: '本地服务',
		api_style: 'llama.cpp',
		provider: 'llama.cpp',
		base_url: 'http://127.0.0.1:8080/v1',
		...BEARER,
		docs_url: 'https://github.com/ggml-org/llama.cpp',
		hint: '本地 server；通常无需 API Key。',
		match_hosts: ['127.0.0.1:8080', 'localhost:8080'],
		keyless: true,
	},
	{
		value: 'ollama',
		label: 'Ollama',
		group: '本地服务',
		api_style: 'openai-chat',
		provider: 'ollama',
		base_url: 'http://127.0.0.1:11434/v1',
		...BEARER,
		docs_url: 'https://github.com/ollama/ollama/blob/main/docs/openai.md',
		hint: '本地 OpenAI 兼容端点；通常无需 API Key。',
		match_hosts: ['127.0.0.1:11434', 'localhost:11434'],
		keyless: true,
	},
	{
		value: 'lm-studio',
		label: 'LM Studio',
		group: '本地服务',
		api_style: 'openai-chat',
		provider: 'lm-studio',
		base_url: 'http://127.0.0.1:1234/v1',
		...BEARER,
		docs_url: 'https://lmstudio.ai/docs/developer/openai-compat',
		hint: '本地 OpenAI-compatible API；默认免 API Key。',
		match_hosts: ['127.0.0.1:1234', 'localhost:1234'],
		keyless: true,
	},
	{
		value: 'vllm',
		label: 'vLLM',
		group: '本地服务',
		api_style: 'openai-chat',
		provider: 'vllm',
		base_url: 'http://127.0.0.1:8000/v1',
		...BEARER,
		docs_url: 'https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/',
		hint: 'OpenAI-compatible server；API Key 是否必需由服务端启动配置决定。媒体端点默认关闭。',
		match_hosts: ['127.0.0.1:8000', 'localhost:8000'],
	},
	// —— 语音专用 ——
	{
		value: 'deepgram',
		label: 'Deepgram (STT)',
		group: '语音服务',
		api_style: 'deepgram',
		provider: 'deepgram',
		base_url: 'https://api.deepgram.com',
		auth_header_name: 'Authorization',
		auth_header_prefix: 'Token',
		docs_url: 'https://developers.deepgram.com/',
		console_url: 'https://console.deepgram.com/',
		hint: '仅语音转写。请分配给 Audio Model，媒体页 STT 选「音频模型」。',
		match_hosts: ['api.deepgram.com'],
	},
	{
		value: 'assemblyai',
		label: 'AssemblyAI (STT)',
		group: '语音服务',
		api_style: 'assemblyai',
		provider: 'assemblyai',
		base_url: 'https://api.assemblyai.com',
		auth_header_name: 'authorization',
		auth_header_prefix: '',
		docs_url: 'https://www.assemblyai.com/docs',
		console_url: 'https://www.assemblyai.com/app/account',
		hint: '仅语音转写。请分配给 Audio Model，媒体页 STT 选「音频模型」。',
		match_hosts: ['api.assemblyai.com'],
	},
	{
		value: 'elevenlabs',
		label: 'ElevenLabs (TTS)',
		group: '语音服务',
		api_style: 'elevenlabs',
		provider: 'elevenlabs',
		base_url: 'https://api.elevenlabs.io',
		auth_header_name: 'xi-api-key',
		auth_header_prefix: '',
		docs_url: 'https://elevenlabs.io/docs',
		console_url: 'https://elevenlabs.io/app/settings/api-keys',
		hint: '仅 TTS。在媒体页语音输出选此 Provider，并填写 Voice ID。',
		match_hosts: ['api.elevenlabs.io'],
	},
];

/** @type {Map<string, ProviderPreset>} */
const PRESET_BY_VALUE = new Map(PROVIDER_PRESETS.map((p) => [p.value, p]));

export function providerPreset(uiStyle: string): ProviderPreset {
	const preset = PRESET_BY_VALUE.get(uiStyle);
	if (!preset) {
		throw new Error(`Unknown provider preset: ${uiStyle}`);
	}
	return preset;
}

function normalizePresetUrl(url: string): string {
	return String(url || '')
		.trim()
		.replace(/\/+$/, '')
		.toLowerCase();
}

/**
 * Apply a provider preset while keeping its identity separate from the wire
 * protocol. Existing custom URLs/capability choices are retained.
 * Never touches `api_key`.
 */
export function applyProviderPreset(
	form: {
		preset: string;
		api_style: string;
		base_url: string;
		api_key?: string;
		media_capabilities: ProviderMediaCapabilitySettings;
	},
	nextStyle: string,
): void {
	const prev = providerPreset(form.preset);
	const next = providerPreset(nextStyle);
	const current = normalizePresetUrl(form.base_url);
	const prevDefault = normalizePresetUrl(prev.base_url);
	const currentCapabilities = form.media_capabilities;
	const previousCapabilities = prev.media_capabilities || defaultProviderMediaCapabilities(prev);
	const nextCapabilities = next.media_capabilities || defaultProviderMediaCapabilities(next);
	const stillPresetCapabilities =
		currentCapabilities.stt === previousCapabilities.stt &&
		currentCapabilities.tts === previousCapabilities.tts &&
		currentCapabilities.image_gen === previousCapabilities.image_gen;
	form.preset = next.value;
	form.api_style = next.api_style;
	if (stillPresetCapabilities) form.media_capabilities = { ...nextCapabilities };
	if (!current || current === prevDefault) {
		form.base_url = next.base_url;
	}
}

/**
 * Options for the Provider preset select (grouped).
 * @type {{ value: string, label: string, group: string }[]}
 */
export const PROVIDER_PRESET_OPTIONS = PROVIDER_PRESETS.map((p) => ({
	value: p.value,
	label: p.label,
	group: p.group,
}));

/**
 * Exact or DNS-suffix host match (no substring `includes`).
 */
function hostMatches(host: string | null | undefined, needles: string[]): boolean {
	if (!host) return false;
	const h = host.toLowerCase();
	return needles.some((n) => {
		const needle = String(n || '')
			.trim()
			.toLowerCase();
		if (!needle) return false;
		return h === needle || h.endsWith(`.${needle}`);
	});
}

/**
 * Resolve which UI preset a saved provider should show.
 * Prefers stored `provider` + wire style; host inference is used only for
 * generic `provider=openai` configurations pointing at a known vendor URL.
 */
export function resolveProviderPreset(p: ProviderStyleInput | null | undefined): string {
	if (!p) return 'openai-chat';
	const style = providerWireStyle(p);
	const provider = String(p.provider || '')
		.trim()
		.toLowerCase();
	const base = String(p.base_url || '')
		.trim()
		.toLowerCase();

	// DeepSeek has two presets on the same vendor hint.
	if (style === 'openai-responses' && provider.includes('deepseek')) {
		return 'deepseek-responses';
	}
	if (style === 'openai-chat' && provider.includes('deepseek')) {
		return 'deepseek-chat';
	}
	if (provider.includes('openrouter')) return 'openrouter';
	if (provider.includes('dashscope')) {
		return style === 'openai-responses' ? 'dashscope-responses' : 'dashscope';
	}

	/** @type {string | null} */
	let host = null;
	try {
		if (base) host = new URL(base).host.toLowerCase();
	} catch {
		host = null;
	}
	// A generic OpenAI provider may point at a known vendor host; surface that
	// vendor's preset so its provider-specific search capability is available.
	if (provider === 'openai') {
		const byVendorHost = PROVIDER_PRESETS.find(
			(pr) => pr.provider !== 'openai' && hostMatches(host, pr.match_hosts || []),
		);
		if (byVendorHost) return byVendorHost.value;
	}

	const sameWire = PROVIDER_PRESETS.filter((pr) => normalizeApiStyle(pr.api_style) === style);

	// Non-generic vendor hints map 1:1 to a preset on the same wire style.
	if (provider && provider !== 'openai') {
		const byProvider = sameWire.find((pr) => pr.provider.toLowerCase() === provider);
		if (byProvider) return byProvider.value;
	}

	const byHost = sameWire.find((pr) => hostMatches(host, pr.match_hosts || []));
	if (byHost) return byHost.value;

	if (style === 'openai-responses') return 'openai-responses';
	if (style === 'openai-chat') return 'openai-chat';
	return style || 'openai-chat';
}

/**
 * True when the provider can be used without an API key (local servers).
 */
export function isKeylessProvider(
	p: Pick<ProviderStyleInput, 'api_style' | 'provider'> | null | undefined,
): boolean {
	if (!p) return false;
	const provider = String(p.provider || '').toLowerCase();
	const style = providerWireStyle(p);
	return (
		style === 'llama.cpp' ||
		PROVIDER_PRESETS.some(
			(preset) => preset.keyless && preset.provider.toLowerCase() === provider,
		)
	);
}
