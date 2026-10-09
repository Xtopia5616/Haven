import { describe, it, expect } from 'vitest';
import { newMessage } from './messageFactory.ts';

describe('newMessage', () => {
	it('builds a message with default type and voice', () => {
		const msg = newMessage({ role: 'assistant' as const, content: 'hi' });
		expect(msg.role).toBe('assistant');
		expect(msg.content).toBe('hi');
		expect(msg.type).toBeNull();
		expect(msg.voice).toBe(false);
		expect(typeof msg.id).toBe('string');
		expect(msg.time).toBeTruthy();
	});

	it('generates unique ids', () => {
		const a = newMessage({ role: 'user' as const, content: 'x' });
		const b = newMessage({ role: 'user' as const, content: 'x' });
		expect(a.id).not.toBe(b.id);
	});

	it('idPrefix slots into the id between timestamp and randomness', () => {
		const msg = newMessage({ role: 'user' as const, content: 'x', idPrefix: 'u' });
		expect(msg.id).toMatch(/^\d+-u-[a-z0-9]+$/);
	});

	it('keeps attachments and overrides time and voice', () => {
		const msg = newMessage({
			role: 'user' as const,
			content: 'x',
			voice: true,
			time: '12:00:00',
			attachments: [{ media_type: 'image/png', data: 'a' }],
		});
		expect(msg.voice).toBe(true);
		expect(msg.time).toBe('12:00:00');
		expect(msg.attachments).toEqual([{ media_type: 'image/png', data: 'a' }]);
	});
});
