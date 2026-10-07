import { formatMessageTime } from './messageFormat.ts';
import type { CanonicalRole } from './contracts/generatedCommands.ts';

export type NewMessageOptions = {
	role: CanonicalRole;
	content: string;
	type?: string | null;
	voice?: boolean;
	time?: string | null;
	attachments?: Array<{ media_type: string; data: string }>;
	idPrefix?: string;
};

export function newMessage({
	role,
	content,
	type = null,
	voice = false,
	time = null,
	attachments = [],
	idPrefix = '',
}: NewMessageOptions) {
	return {
		id: `${Date.now()}${idPrefix ? `-${idPrefix}` : ''}-${Math.random().toString(36).slice(2, 6)}`,
		role,
		content,
		type,
		voice,
		time: time || formatMessageTime(new Date()),
		attachments,
	};
}
