/** Capability/request policy metadata shared by the model settings view. */

export const capabilityOptions = [
	{ value: 'chat', label: 'Chat' },
	{ value: 'fast_chat', label: 'Fast chat' },
	{ value: 'vision', label: 'Vision' },
	{ value: 'audio_input', label: 'Audio input' },
	{ value: 'transcription', label: 'Transcription' },
	{ value: 'embedding', label: 'Embedding' },
	{ value: 'image_generation', label: 'Image generation' },
	{ value: 'speech_synthesis', label: 'Speech synthesis' },
];

export const requestPolicyOptions = [
	{ value: 'chat', label: 'Chat' },
	{ value: 'fast_chat', label: 'Fast chat' },
	{ value: 'vision', label: 'Vision' },
	{ value: 'audio_chat', label: 'Audio chat' },
	{ value: 'transcription', label: 'Transcription' },
	{ value: 'embedding', label: 'Embedding' },
	{ value: 'image_generation', label: 'Image generation' },
	{ value: 'speech_synthesis', label: 'Speech synthesis' },
];

export function emptyModel(id: string) {
	return {
		id,
		provider: '',
		model: '',
		capabilities: [],
		temperature: null as number | null,
		context_window: null as number | null,
		cost_per_1k_input_tokens: null as number | null,
		cost_per_1k_output_tokens: null as number | null,
		cost_per_1k_cache_read_tokens: null as number | null,
		cost_per_1k_cache_write_tokens: null as number | null,
	};
}
