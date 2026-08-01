import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import ChatBubble from './ChatBubble.svelte';

describe('ChatBubble', () => {
	const base = (props) => ({ type: null, time: null, ...props });

	it('renders a user bubble with the You label', () => {
		render(ChatBubble, base({ role: 'user', content: 'hello there' }));
		expect(screen.getByText('You')).toBeTruthy();
		expect(screen.getByText('hello there')).toBeTruthy();
	});

	it('renders image attachments on user messages', () => {
		const { container } = render(
			ChatBubble,
			base({
				role: 'user',
				content: '看图',
				attachments: [{ media_type: 'image/png', data: 'aGVsbG8=' }],
			}),
		);
		const imgs = container.querySelectorAll('.attachment-img');
		expect(imgs.length).toBe(1);
		expect(imgs[0].getAttribute('src')).toBe('data:image/png;base64,aGVsbG8=');
		expect(screen.getByText('看图')).toBeTruthy();
	});

	it('renders multiple image attachments', () => {
		const { container } = render(
			ChatBubble,
			base({
				role: 'user',
				content: '',
				attachments: [
					{ media_type: 'image/png', data: 'A' },
					{ media_type: 'image/jpeg', data: 'B' },
				],
			}),
		);
		expect(container.querySelectorAll('.attachment-img').length).toBe(2);
	});

	it('renders an assistant bubble with the Haven label', () => {
		render(ChatBubble, base({ role: 'assistant', content: 'hi' }));
		expect(screen.getByText('Haven')).toBeTruthy();
	});

	it('shows the voice icon for voice input', () => {
		render(ChatBubble, base({ role: 'user', content: 'hi', voice: true }));
		expect(screen.getByTitle('Voice input')).toBeTruthy();
	});

	it('shows the time when provided', () => {
		render(ChatBubble, base({ role: 'user', content: 'hi', time: '10:30' }));
		expect(screen.getByText('10:30')).toBeTruthy();
	});

	it('renders thought messages as italic text', () => {
		render(ChatBubble, base({ role: 'assistant', content: 'thinking hard', type: 'thought' }));
		const em = document.querySelector('em.thought');
		expect(em).toBeTruthy();
		expect(em.textContent).toBe('thinking hard');
	});

	it('renders a caret while a thought is streaming', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'live',
				type: 'thought',
				streaming: true,
			}),
		);
		expect(document.querySelector('em.thought .caret')).toBeTruthy();
	});

	it('renders the reasoning block expanded while streaming', async () => {
		const { container } = render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'chain of thought',
				type: 'reasoning',
				streaming: true,
			}),
		);
		const details = /** @type {HTMLDetailsElement} */ (
			container.querySelector('details.reasoning-block')
		);
		expect(details).toBeTruthy();
		expect(details.open).toBe(true);
		expect(details.textContent).toContain('chain of thought');
	});

	it('auto-collapses the reasoning block when streaming ends', async () => {
		const { container, rerender } = render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'coiled',
				type: 'reasoning',
				streaming: true,
			}),
		);
		const details = /** @type {HTMLDetailsElement} */ (
			container.querySelector('details.reasoning-block')
		);
		expect(details.open).toBe(true);
		await rerender({ streaming: false });
		expect(details.open).toBe(false);
	});

	it('does not clobber a manual re-open after streaming ends', async () => {
		const { container, rerender } = render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'first',
				type: 'reasoning',
				streaming: true,
			}),
		);
		const details = /** @type {HTMLDetailsElement} */ (
			container.querySelector('details.reasoning-block')
		);
		await rerender({ streaming: false });
		expect(details.open).toBe(false);

		// User manually re-opens the block.
		details.open = true;
		// A content update without a streaming transition must not reset it.
		await rerender({ content: 'second' });
		expect(details.open).toBe(true);
	});

	it('renders a tool call with the tool name and expandable observation', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'stdout output',
				type: 'tool',
				toolName: 'shell',
			}),
		);
		expect(screen.getByText('▶ Calling shell')).toBeTruthy();
		const details = document.querySelector('details.observation-block');
		expect(details).toBeTruthy();
		expect(details.textContent).toContain('stdout output');
	});

	it('omits the observation block when a tool call has no content', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: '',
				type: 'tool',
				toolName: 'shell',
			}),
		);
		expect(screen.getByText('▶ Calling shell')).toBeTruthy();
		expect(document.querySelector('details.observation-block')).toBeNull();
	});

	it('renders supplement messages as a badge', () => {
		render(ChatBubble, base({ role: 'assistant', content: 'extra context', type: 'supplement' }));
		expect(document.querySelector('.supplement-badge')).toBeTruthy();
		expect(document.querySelector('.supplement-badge').textContent).toContain('extra context');
	});

	it('calls onContextMenu with bubble metadata', async () => {
		const onContextMenu = vi.fn();
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'ctx',
				messageId: 'm42',
				stepNumber: 7,
				type: 'thought',
				onContextMenu,
			}),
		);
		await fireEvent.contextMenu(document.querySelector('.bubble'), {
			clientX: 11,
			clientY: 22,
		});
		expect(onContextMenu).toHaveBeenCalledTimes(1);
		const payload = onContextMenu.mock.calls[0][0];
		expect(payload).toMatchObject({
			x: 11,
			y: 22,
			messageId: 'm42',
			stepNumber: 7,
			role: 'assistant',
			content: 'ctx',
			type: 'thought',
		});
		expect(typeof payload.selectedContent).toBe('string');
	});

	it('does not call onContextMenu when no handler is given', async () => {
		render(ChatBubble, base({ role: 'user', content: 'plain' }));
		await fireEvent.contextMenu(document.querySelector('.bubble'));
	});
});
