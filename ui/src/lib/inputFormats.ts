// Canonical media-channel metadata for the settings「媒体」page.
// Mirrors chat modalities (text / image / file / voice) so cards and copy
// stay in one place; per-channel field widgets stay in ModelSettings.

/** @typedef {'text' | 'image' | 'file' | 'voice'} InputFormatId */

/**
 * @typedef {object} InputFormatCard
 * @property {InputFormatId} id
 * @property {string} label
 * @property {string} hint — static description; dynamic bits (limits) are
 *   appended in the card body when needed.
 */

/** Single source of truth for the media-channel cards (voice first). */
export const inputFormats = [
	{
		id: 'voice',
		label: '语音 Voice',
		hint: '输入：热键录音 → STT 转写为文本。输出：朗读/配音走 TTS。',
	},
	{
		id: 'image',
		label: '图片 Image',
		hint: '输入：附件压缩后交视觉模型理解，文字提取走 OCR。输出：文生图。',
	},
	{
		id: 'file',
		label: '文件 File',
		hint: '音频附件以内联媒体交给 Audio Model；其他文件后端落盘后由 agent 通过 files 工具读取。',
	},
	{
		id: 'text',
		label: '文本 Text',
		hint: '文字指令直接发给 Default Model；语音转写结果也进入同一通道，无需额外配置。',
	},
];
