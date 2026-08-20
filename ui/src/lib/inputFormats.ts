// Canonical input-channel metadata for the settings「输入」page.
// Mirrors the chat input modalities (text / image / file / voice) so cards
// and copy stay in one place; per-format field widgets stay in ModelSettings.

/** @typedef {'text' | 'image' | 'file' | 'voice'} InputFormatId */

/**
 * @typedef {object} InputFormatCard
 * @property {InputFormatId} id
 * @property {string} label
 * @property {string} hint — static description; dynamic bits (limits) are
 *   appended in the card body when needed.
 */

/** Single source of truth for the input-format cards. */
export const inputFormats = [
	{
		id: 'text',
		label: '文本 Text',
		hint: '文字指令直接发送给 Default Model 处理。语音转写结果也以文本形式进入同一通道，无需额外配置。',
	},
	{
		id: 'image',
		label: '图片 Image',
		hint: '粘贴或选取的图片先压缩为 JPEG，再交由视觉模型理解；关闭专用模型后改由 Default Model 处理。',
	},
	{
		id: 'file',
		label: '文件 File',
		hint: '附件以 base64 上传，后端保存到磁盘，agent 通过 file 工具读取路径进行处理，无需额外配置。',
	},
	{
		id: 'voice',
		label: '语音 Voice',
		hint: '按住热键录音，经 STT 转写为文本后作为普通消息发送；转写可走专用音频模型或使用 Default Model。',
	},
];
