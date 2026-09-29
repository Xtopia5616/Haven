/** Capability/request policy metadata shared by the model settings view. */

export const requestKindValues = [
	'chat',
	'fast_chat',
	'vision',
	'audio_chat',
	'transcription',
	'embedding',
	'image_generation',
	'speech_synthesis',
] as const;

export type RequestKind = (typeof requestKindValues)[number];

export const capabilityOptions = [
	{ value: 'chat', label: '对话' },
	{ value: 'fast_chat', label: '快速对话' },
	{ value: 'vision', label: '视觉' },
	{ value: 'audio_input', label: '音频输入' },
	{ value: 'transcription', label: '语音转写' },
	{ value: 'embedding', label: '向量嵌入' },
	{ value: 'image_generation', label: '图像生成' },
	{ value: 'speech_synthesis', label: '语音合成' },
];

export const requestPolicyOptions: Array<{ value: RequestKind; label: string }> = [
	{ value: 'chat', label: '对话' },
	{ value: 'fast_chat', label: '快速对话' },
	{ value: 'vision', label: '视觉理解' },
	{ value: 'audio_chat', label: '音频对话' },
	{ value: 'transcription', label: '语音转写' },
	{ value: 'embedding', label: '向量嵌入' },
	{ value: 'image_generation', label: '图像生成' },
	{ value: 'speech_synthesis', label: '语音合成' },
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
