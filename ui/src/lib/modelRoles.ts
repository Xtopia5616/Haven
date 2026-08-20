// Canonical model-role metadata, mirroring the backend EndpointRole list
// (haven_common::config::EndpointRole::ALL). Adding/renaming a model slot
// here renders its card everywhere without touching per-card markup.
// NOTE: `audio_model` keeps a bespoke inline card (STT provider handling);
// all other roles share the generic role picker markup.

/** The six role keys, in the backend's canonical order. */
export const ROLE_KEYS = [
	'default_model',
	'balanced_model',
	'small_model',
	'image_model',
	'embedding_model',
	'audio_model',
];

/** Single source of truth for the LLM endpoint cards. */
export const modelCards = [
	{ key: 'default_model', label: 'Default Model', hint: 'Primary reasoning & tool-use agent', prefix: 'dm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
	{ key: 'balanced_model', label: 'Balanced Model', hint: 'Used when Default Model is unavailable', prefix: 'bm', basePlaceholder: 'http://localhost:11434', group: 'core' },
	{ key: 'small_model', label: 'Small Model', hint: 'Title generation & lightweight reasoning', prefix: 'sm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
	{ key: 'image_model', label: 'Image Model', hint: 'Image understanding (vision-capable)', prefix: 'im', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	{ key: 'audio_model', label: 'Audio Model', hint: 'Speech-to-text (Whisper / Gemini / Deepgram / AssemblyAI, or multimodal chat fallback)', prefix: 'au', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	{ key: 'embedding_model', label: 'Embedding Model', hint: 'Semantic memory: vectors for facts & past conversations. Local (Ollama / LM Studio) or cloud', prefix: 'em', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
];

export const coreModelCards = modelCards.filter((c) => c.group === 'core');
export const specializedModelCards = modelCards.filter((c) => c.group === 'specialized');