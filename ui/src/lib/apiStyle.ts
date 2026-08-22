/**
 * Wire-protocol (`api_style`) helpers mirrored from
 * `haven_llm::adapters::capabilities`. Chat 「联网搜索」 stays off|auto|always;
 * unsupported styles should grey out / ignore the control.
 */

/** @param {string | null | undefined} style */
export function isKnownApiStyle(style) {
	const s = String(style || '')
		.trim()
		.toLowerCase();
	return (
		s === 'openai-responses' ||
		s === 'deepseek-responses' ||
		s === 'responses' ||
		s === 'openai-chat' ||
		s === 'openai' ||
		s === 'chat' ||
		s === 'llama.cpp' ||
		s === 'llama' ||
		s === 'llamacpp' ||
		s === 'xai' ||
		s === 'grok' ||
		s === 'anthropic' ||
		s === 'claude' ||
		s === 'gemini' ||
		s === 'google' ||
		s === 'deepgram' ||
		s === 'assemblyai'
	);
}

/** @param {string | null | undefined} style */
export function normalizeApiStyle(style) {
	const s = String(style || '')
		.trim()
		.toLowerCase();
	switch (s) {
		case 'openai-responses':
		case 'deepseek-responses':
		case 'responses':
			return 'openai-responses';
		case 'openai-chat':
		case 'openai':
		case 'chat':
			return 'openai-chat';
		case 'llama.cpp':
		case 'llama':
		case 'llamacpp':
			return 'llama.cpp';
		case 'xai':
		case 'grok':
			return 'xai';
		case 'anthropic':
		case 'claude':
			return 'anthropic';
		case 'gemini':
		case 'google':
			return 'gemini';
		case 'deepgram':
			return 'deepgram';
		case 'assemblyai':
			return 'assemblyai';
		default:
			return 'openai-chat';
	}
}

/** @param {string | null | undefined} style */
export function supportsBuiltinWebSearch(style) {
	const n = normalizeApiStyle(style);
	return (
		n === 'openai-responses' || n === 'xai' || n === 'anthropic' || n === 'gemini'
	);
}

/** @param {string | null | undefined} style */
export function isSttOnlyStyle(style) {
	const n = normalizeApiStyle(style);
	return n === 'deepgram' || n === 'assemblyai';
}

/**
 * Sampling / structured fields mirrored from
 * `haven_common::config::supports_sampling_field`.
 * @typedef {'temperature'|'top_p'|'top_k'|'frequency_penalty'|'presence_penalty'|'stop'|'seed'|'response_format'|'reasoning_effort'} SamplingField
 */

/** @type {SamplingField[]} */
export const SAMPLING_FIELDS = [
	'temperature',
	'top_p',
	'top_k',
	'frequency_penalty',
	'presence_penalty',
	'stop',
	'seed',
	'response_format',
	'reasoning_effort',
];

/**
 * @param {string | null | undefined} style
 * @param {SamplingField | string} field
 */
export function supportsSamplingField(style, field) {
	const n = normalizeApiStyle(style);
	if (n === 'deepgram' || n === 'assemblyai') return false;
	const f = String(field || '');
	switch (f) {
		case 'temperature':
		case 'top_p':
			return (
				n === 'openai-chat' ||
				n === 'llama.cpp' ||
				n === 'xai' ||
				n === 'openai-responses' ||
				n === 'anthropic' ||
				n === 'gemini'
			);
		case 'top_k':
		case 'stop':
			return (
				n === 'openai-chat' ||
				n === 'llama.cpp' ||
				n === 'xai' ||
				n === 'anthropic' ||
				n === 'gemini'
			);
		case 'frequency_penalty':
		case 'presence_penalty':
		case 'response_format':
			return n === 'openai-chat' || n === 'llama.cpp' || n === 'xai';
		case 'seed':
			return n === 'openai-chat' || n === 'llama.cpp' || n === 'xai' || n === 'gemini';
		case 'reasoning_effort':
			return (
				n === 'openai-chat' ||
				n === 'llama.cpp' ||
				n === 'xai' ||
				n === 'openai-responses' ||
				n === 'anthropic'
			);
		default:
			return false;
	}
}

/**
 * Human-readable list of sampling fields the style forwards (for settings hints).
 * @param {string | null | undefined} style
 */
export function samplingFieldsHint(style) {
	const supported = SAMPLING_FIELDS.filter((f) => supportsSamplingField(style, f));
	if (!supported.length) return '当前线协议不转发采样/结构化字段';
	return `当前线协议可转发：${supported.join('、')}`;
}

/**
 * UI display style for a saved provider: DeepSeek Responses is stored as
 * `openai-responses` + `provider=deepseek`, but the settings select shows the
 * dedicated preset value.
 * @param {{ api_style?: string | null, provider?: string | null } | null | undefined} p
 */
export function displayApiStyle(p) {
	const style = p?.api_style || '';
	const provider = String(p?.provider || '').toLowerCase();
	if (
		normalizeApiStyle(style) === 'openai-responses' &&
		(provider.includes('deepseek') || style === 'deepseek-responses')
	) {
		return 'deepseek-responses';
	}
	return style || 'openai-chat';
}

/**
 * Persist shape for a selected UI style option.
 * @param {string} uiStyle
 * @returns {{ api_style: string, provider: string, base_url: string, auth_header_name: string, auth_header_prefix: string }}
 */
export function apiStylePreset(uiStyle) {
	const presets = {
		'openai-chat': {
			api_style: 'openai-chat',
			provider: 'openai',
			base_url: 'https://api.openai.com/v1',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		'llama.cpp': {
			api_style: 'llama.cpp',
			provider: 'llama.cpp',
			base_url: 'http://127.0.0.1:8080/v1',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		'openai-responses': {
			api_style: 'openai-responses',
			provider: 'openai',
			base_url: 'https://api.openai.com/v1',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		'deepseek-responses': {
			api_style: 'openai-responses',
			provider: 'deepseek',
			base_url: 'https://api.deepseek.com',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		xai: {
			api_style: 'xai',
			provider: 'xai',
			base_url: 'https://api.x.ai/v1',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Bearer',
		},
		anthropic: {
			api_style: 'anthropic',
			provider: 'anthropic',
			base_url: 'https://api.anthropic.com',
			auth_header_name: 'x-api-key',
			auth_header_prefix: '',
		},
		gemini: {
			api_style: 'gemini',
			provider: 'gemini',
			base_url: 'https://generativelanguage.googleapis.com/v1beta',
			auth_header_name: 'x-goog-api-key',
			auth_header_prefix: '',
		},
		deepgram: {
			api_style: 'deepgram',
			provider: 'deepgram',
			base_url: 'https://api.deepgram.com',
			auth_header_name: 'Authorization',
			auth_header_prefix: 'Token',
		},
		assemblyai: {
			api_style: 'assemblyai',
			provider: 'assemblyai',
			base_url: 'https://api.assemblyai.com',
			auth_header_name: 'authorization',
			auth_header_prefix: '',
		},
	};
	return presets[uiStyle] || presets['openai-chat'];
}

export const API_STYLE_OPTIONS = [
	{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
	{ value: 'llama.cpp', label: 'llama.cpp server (local)' },
	{ value: 'openai-responses', label: 'OpenAI Responses' },
	{ value: 'deepseek-responses', label: 'DeepSeek Responses (思考+联网)' },
	{ value: 'xai', label: 'xAI Grok (Live Search)' },
	{ value: 'anthropic', label: 'Anthropic (Claude)' },
	{ value: 'gemini', label: 'Google Gemini' },
	{ value: 'deepgram', label: 'Deepgram (STT only)' },
	{ value: 'assemblyai', label: 'AssemblyAI (STT only)' },
];
