// Canonical model-role metadata, mirroring the backend EndpointRole list
// (haven_common::config::EndpointRole::ALL). Adding/renaming a model slot
// here renders its card everywhere without touching per-card markup.
// STT / OCR live on the Voice / Image input cards, not on role pickers.

/** The six role keys, in the backend's canonical order. */
export const ROLE_KEYS = [
	'default_model',
	'balanced_model',
	'small_model',
	'image_model',
	'embedding_model',
	'audio_model',
];

/** Empty role slot used when materializing missing pickers. */
export function emptyRoleSlot(key: string) {
	return {
		role: key,
		provider: '',
		model: '',
		temperature: null as number | null,
		context_window: null as number | null,
		cost_per_1k_input_tokens: null as number | null,
		cost_per_1k_output_tokens: null as number | null,
	};
}

/**
 * Ensure every ROLE_KEYS slot exists on the shared roles array (mutate in place).
 * Call before the dirty snapshot so ModelSettings mount effects are not a false edit.
 */
export function ensureRoleSlots(roles: { role: string }[]) {
	if (!Array.isArray(roles)) return;
	for (const key of ROLE_KEYS) {
		if (!roles.some((r) => r.role === key)) {
			roles.push(emptyRoleSlot(key));
		}
	}
}

/** Single source of truth for the LLM endpoint cards. */
export const modelCards = [
	{ key: 'default_model', label: 'Default Model', hint: 'Primary reasoning & tool-use agent', prefix: 'dm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
	{ key: 'balanced_model', label: 'Balanced Model', hint: 'Used when Default Model is unavailable', prefix: 'bm', basePlaceholder: 'http://localhost:11434', group: 'core' },
	{ key: 'small_model', label: 'Small Model', hint: 'Title generation & lightweight reasoning', prefix: 'sm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
	{ key: 'image_model', label: 'Image Model', hint: 'Vision + OCR fallback（理解与文字提取）', prefix: 'im', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	{ key: 'audio_model', label: 'Audio Model', hint: 'STT 首选端点（Whisper / Gemini / Deepgram / AssemblyAI，或 multimodal chat）', prefix: 'au', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	{ key: 'embedding_model', label: 'Embedding Model', hint: 'Semantic memory. OpenAI-compatible /v1/embeddings, Gemini embedding models, or local (Ollama / LM Studio). Not Chat Completions.', prefix: 'em', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
];

export const coreModelCards = modelCards.filter((c) => c.group === 'core');
export const specializedModelCards = modelCards.filter((c) => c.group === 'specialized');