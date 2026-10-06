/** Capability/request policy metadata shared by the model settings view. */

import {
	REQUEST_KIND_VALUES,
	type CapabilityInput,
	type RequestKind,
} from './contracts/generatedCommands.ts';

const requestKindLabels: Record<RequestKind, string> = {
	chat: '对话',
	fast_chat: '轻量任务',
	vision: '视觉理解',
	audio_chat: '音频对话',
	transcription: '语音转写',
	embedding: '向量嵌入',
	image_generation: '图像生成',
	speech_synthesis: '语音合成',
};

export const capabilityOptions: Array<{ value: CapabilityInput; label: string }> = [
	{ value: 'chat', label: '对话' },
	{ value: 'fast_chat', label: '轻量任务' },
	{ value: 'vision', label: '视觉' },
	{ value: 'audio_input', label: '音频输入' },
	{ value: 'transcription', label: '语音转写' },
	{ value: 'embedding', label: '向量嵌入' },
	{ value: 'image_generation', label: '图像生成' },
	{ value: 'speech_synthesis', label: '语音合成' },
];

export const requestPolicyOptions = REQUEST_KIND_VALUES.map((value) => ({
	value,
	label: requestKindLabels[value],
}));

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
