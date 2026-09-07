import { render } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import ConversationTimeline from './ConversationTimeline.svelte';

describe('ConversationTimeline', () => {
	it('uses the shared page loader while the message timeline is loading', () => {
		render(ConversationTimeline, {
			messages: [{ id: 'msg-1', role: 'user', content: '你好', type: 'user' }],
		});

		expect(document.querySelector('.loading-state--page')).toBeTruthy();
		expect(document.querySelector('.loading-state--inline')).toBeNull();
	});
});
