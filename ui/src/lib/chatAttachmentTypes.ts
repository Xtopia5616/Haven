import type { MessageAttachmentInput } from './contracts/generatedCommands.ts';

/** Inline media bytes accepted by the chat transcript submission path. */
export type ChatAttachmentPayload = Pick<MessageAttachmentInput, 'media_type' | 'data'>;

/** Named file bytes accepted by the chat transcript submission path. */
export interface ChatFileAttachment extends ChatAttachmentPayload {
	filename: string;
}
