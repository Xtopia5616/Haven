/** UI mirror of Common's canonical wire-style and media-endpoint rules. */
import type { ProviderConfigInput } from './contracts/generatedCommands.ts';

export interface ProviderMediaCapabilitySettings {
	stt: boolean;
	tts: boolean;
	image_gen: boolean;
}

/** The provider settings fields used for wire-style and capability hints. */
export type ProviderStyleInput = Pick<ProviderConfigInput, 'api_style' | 'media_capabilities'> & {
	provider?: string | null;
	base_url?: string | null;
};

export function isKnownApiStyle(style: string | null | undefined): boolean {
	const s = String(style || '')
		.trim()
		.toLowerCase();
	return (
		s === 'openai-responses' ||
		s === 'openai-chat' ||
		s === 'llama.cpp' ||
		s === 'xai' ||
		s === 'openrouter' ||
		s === 'dashscope' ||
		s === 'anthropic' ||
		s === 'gemini' ||
		s === 'deepgram' ||
		s === 'assemblyai' ||
		s === 'elevenlabs'
	);
}

export function normalizeApiStyle(style: string | null | undefined): string {
	const s = String(style || '')
		.trim()
		.toLowerCase();
	switch (s) {
		case 'openai-responses':
			return 'openai-responses';
		case 'openai-chat':
			return 'openai-chat';
		case 'llama.cpp':
			return 'llama.cpp';
		case 'xai':
			return 'xai';
		case 'openrouter':
			return 'openrouter';
		case 'dashscope':
			return 'dashscope';
		case 'anthropic':
			return 'anthropic';
		case 'gemini':
			return 'gemini';
		case 'deepgram':
			return 'deepgram';
		case 'assemblyai':
			return 'assemblyai';
		case 'elevenlabs':
			return 'elevenlabs';
		default:
			return 'invalid';
	}
}

/**
 * Effective wire style for a saved provider. An omitted style uses the
 * neutral OpenAI-compatible protocol; vendor identity is not a protocol
 * selector.
 */
export function providerWireStyle(
	p: Pick<ProviderStyleInput, 'api_style'> | null | undefined,
): string {
	const raw = String(p?.api_style || '').trim();
	if (raw) return normalizeApiStyle(raw);
	return 'openai-chat';
}

/** Mirror Rust `is_openai_family_wire_style`. */
export function isOpenaiFamilyWireStyle(style: string | null | undefined): boolean {
	const n = normalizeApiStyle(style);
	return (
		n === 'openai-chat' ||
		n === 'openai-responses' ||
		n === 'llama.cpp' ||
		n === 'xai' ||
		n === 'openrouter' ||
		n === 'dashscope'
	);
}

/** Mirror Rust `is_tts_only_style`. */
export function isTtsOnlyStyle(style: string | null | undefined): boolean {
	return normalizeApiStyle(style) === 'elevenlabs';
}

/** Routes Haven knows how to call for a given protocol style. */
export function mediaCapabilitiesSupportedByStyle(
	style: string | null | undefined,
): ProviderMediaCapabilitySettings {
	if (isOpenaiFamilyWireStyle(style)) return { stt: true, tts: true, image_gen: true };
	switch (normalizeApiStyle(style)) {
		case 'gemini':
			return { stt: true, tts: false, image_gen: true };
		case 'deepgram':
		case 'assemblyai':
			return { stt: true, tts: false, image_gen: false };
		case 'elevenlabs':
			return { stt: false, tts: true, image_gen: false };
		default:
			return { stt: false, tts: false, image_gen: false };
	}
}

/**
 * Conservative defaults for existing configs. A compatible chat protocol
 * does not imply that the service implements separate media endpoints.
 */
export function defaultProviderMediaCapabilities(
	p: ProviderStyleInput | null | undefined,
): ProviderMediaCapabilitySettings {
	if (!p) return { stt: false, tts: false, image_gen: false };
	const provider = String(p.provider || '')
		.trim()
		.toLowerCase();
	const style = providerWireStyle(p);
	let host = '';
	try {
		host = new URL(String(p.base_url || '')).hostname.toLowerCase();
	} catch {
		host = '';
	}
	if (style === 'elevenlabs' || provider === 'elevenlabs')
		return { stt: false, tts: true, image_gen: false };
	if (style === 'gemini') return { stt: true, tts: false, image_gen: true };
	if (style === 'deepgram' || style === 'assemblyai')
		return { stt: true, tts: false, image_gen: false };
	if (isOpenaiFamilyWireStyle(style)) {
		if (provider === 'groq' || host === 'groq.com' || host.endsWith('.groq.com'))
			return { stt: true, tts: false, image_gen: false };
		if (host === 'api.openai.com') return mediaCapabilitiesSupportedByStyle(style);
	}
	return { stt: false, tts: false, image_gen: false };
}

/** Explicit values override safe inferred defaults from older configs. */
export function providerMediaCapabilities(
	p: ProviderStyleInput | null | undefined,
): ProviderMediaCapabilitySettings {
	if (p?.media_capabilities) {
		return {
			stt: !!p.media_capabilities.stt,
			tts: !!p.media_capabilities.tts,
			image_gen: !!p.media_capabilities.image_gen,
		};
	}
	return defaultProviderMediaCapabilities(p);
}

/** Mirror backend `tts_backend_for` / `image_gen_backend_for`. */
export function mediaCapabilityBackend(
	p: ProviderStyleInput | null | undefined,
	capability: 'tts' | 'image_gen',
): 'openai' | 'gemini' | 'elevenlabs' | '' {
	if (!p || !providerMediaCapabilities(p)[capability]) return '';
	const provider = String(p.provider || '').toLowerCase();
	const style = providerWireStyle(p);
	if (capability === 'tts' && (style === 'elevenlabs' || provider === 'elevenlabs'))
		return 'elevenlabs';
	if (capability === 'image_gen' && style === 'gemini') return 'gemini';
	return isOpenaiFamilyWireStyle(style) ? 'openai' : '';
}

/** Mirror backend `stt_backend_for`. */
export function sttCapabilityBackend(
	p: ProviderStyleInput | null | undefined,
): 'openai' | 'groq' | 'gemini' | 'deepgram' | 'assemblyai' | '' {
	if (!p || !providerMediaCapabilities(p).stt) return '';
	const provider = String(p.provider || '').toLowerCase();
	const style = providerWireStyle(p);
	if (style === 'deepgram') return 'deepgram';
	if (style === 'assemblyai') return 'assemblyai';
	if (style === 'gemini') return 'gemini';
	if (isOpenaiFamilyWireStyle(style)) {
		const host = String(p.base_url || '').toLowerCase();
		if (provider === 'groq' || host.includes('groq.com')) return 'groq';
		return 'openai';
	}
	return '';
}

export function supportsBuiltinWebSearch(style: string | null | undefined): boolean {
	const n = normalizeApiStyle(style);
	return (
		n === 'openai-chat' ||
		n === 'openai-responses' ||
		n === 'xai' ||
		n === 'openrouter' ||
		n === 'dashscope' ||
		n === 'anthropic' ||
		n === 'gemini'
	);
}

export function isSttOnlyStyle(style: string | null | undefined): boolean {
	const n = normalizeApiStyle(style);
	return n === 'deepgram' || n === 'assemblyai';
}

/** Sampling / structured fields listed in per-wire-style settings hints. */
const SAMPLING_FIELDS = [
	'temperature',
	'top_p',
	'top_k',
	'frequency_penalty',
	'presence_penalty',
	'stop',
	'seed',
	'response_format',
	'reasoning_effort',
] as const;

export function supportsSamplingField(style: string | null | undefined, field: string): boolean {
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
 */
export function samplingFieldsHint(style: string | null | undefined): string {
	const supported = SAMPLING_FIELDS.filter((f) => supportsSamplingField(style, f));
	if (!supported.length) return '当前线协议不转发采样/结构化字段';
	return `当前线协议可转发：${supported.join('、')}`;
}
