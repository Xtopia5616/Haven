/** Image bytes accepted by the chat transcript submission path. */
export interface ChatImageAttachment {
	media_type: string;
	data: string;
}

/** Named file bytes accepted by the chat transcript submission path. */
export interface ChatFileAttachment extends ChatImageAttachment {
	filename: string;
}
