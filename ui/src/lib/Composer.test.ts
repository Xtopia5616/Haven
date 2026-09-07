import { describe, it, expect, vi } from 'vitest';
import { render } from '@testing-library/svelte';
import Composer from './Composer.svelte';

describe('Composer imperative input API', () => {
	it('forwards setDraft to the wrapped InputRouter', async () => {
		const { component, container } = render(Composer, { onsubmit: vi.fn() });
		const input = container.querySelector('textarea') as HTMLTextAreaElement;

		(component as { setDraft: (text: string) => void }).setDraft('恢复后的消息');

		await vi.waitFor(() => expect(input.value).toBe('恢复后的消息'));
	});
});
