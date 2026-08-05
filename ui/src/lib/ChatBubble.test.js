import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/svelte';
import ChatBubble from './ChatBubble.svelte';

describe('ChatBubble', () => {
	const base = (props) => ({ type: null, time: null, ...props });

	it('renders a user bubble with the You label', () => {
		render(ChatBubble, base({ role: 'user', content: 'hello there' }));
		expect(screen.getByText('You')).toBeTruthy();
		expect(screen.getByText('hello there')).toBeTruthy();
	});

	it('shows the mic icon on voice user messages', () => {
		const { container } = render(
			ChatBubble,
			base({ role: 'user', content: 'hello', voice: true }),
		);
		expect(container.querySelector('.mic-icon')).toBeTruthy();
	});

	it('shows the received checkmark only on received user messages', () => {
		const { container: recv } = render(
			ChatBubble,
			base({ role: 'user', content: 'hi', received: true }),
		);
		expect(recv.querySelector('.received-tag')).toBeTruthy();
		const { container: plain } = render(
			ChatBubble,
			base({ role: 'user', content: 'hi', received: false }),
		);
		expect(plain.querySelector('.received-tag')).toBeNull();
		const { container: assistant } = render(
			ChatBubble,
			base({ role: 'assistant', content: 'hi', received: true }),
		);
		expect(assistant.querySelector('.received-tag')).toBeNull();
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

	it('does not apply a pending class to streaming assistant text bubbles', () => {
		const { container } = render(
			ChatBubble,
			base({ role: 'assistant', content: 'hi', streaming: true }),
		);
		const bubble = container.querySelector('.bubble');
		expect(bubble.classList.contains('pending')).toBe(false);
		expect(bubble.classList.contains('assistant')).toBe(true);
	});

	it('does not apply any pending class to finalized assistant bubbles', () => {
		const { container } = render(ChatBubble, base({ role: 'assistant', content: 'hi' }));
		expect(container.querySelector('.bubble').classList.contains('pending')).toBe(false);
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

	it('renders a shell tool call with a terminal output card', () => {
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
		expect(document.querySelector('.tool-card')).toBeTruthy();
		expect(screen.getByText('终端输出')).toBeTruthy();
		expect(screen.getByText('stdout output')).toBeTruthy();
		expect(document.querySelector('details.observation-block')).toBeNull();
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

	it('renders a structured tool result card for JSON observations', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: JSON.stringify({ results: [{ path: 'C:\\a.rs' }], count: 1, mode: 'filename' }),
				type: 'tool',
				toolName: 'search',
			}),
		);
		expect(screen.getByText('▶ Calling search')).toBeTruthy();
		expect(document.querySelector('.tool-card')).toBeTruthy();
		expect(screen.getByText('文件搜索')).toBeTruthy();
		expect(screen.getByText('C:\\a.rs')).toBeTruthy();
		expect(document.querySelector('details.observation-block')).toBeNull();
	});

	it('falls back to the raw observation block for non-JSON text', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: 'some plain error text',
				type: 'tool',
				toolName: 'audio',
			}),
		);
		expect(document.querySelector('.tool-card')).toBeNull();
		const details = document.querySelector('details.observation-block');
		expect(details).toBeTruthy();
		expect(details.textContent).toContain('some plain error text');
	});

	it('renders supplement messages as a badge', () => {
		render(ChatBubble, base({ role: 'assistant', content: 'extra context', type: 'supplement' }));
		expect(document.querySelector('.supplement-badge')).toBeTruthy();
		expect(document.querySelector('.supplement-badge').textContent).toContain('extra context');
	});

	it('renders an ask question card with an awaiting indicator', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: '你想怎么做？',
				type: 'ask',
				awaiting: true,
				options: ['方案 A', '方案 B'],
			}),
		);
		expect(document.querySelector('.tool-card')).toBeTruthy();
		expect(screen.getByText('你想怎么做？')).toBeTruthy();
		expect(screen.getByText('等待你的回答...')).toBeTruthy();
		expect(screen.getByText('方案 A')).toBeTruthy();
		expect(screen.getByText('方案 B')).toBeTruthy();
	});

	it('hides options and awaiting feedback once answered or not awaiting', () => {
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: '已问过',
				type: 'ask',
				awaiting: false,
				options: ['方案 A'],
			}),
		);
		expect(document.querySelector('.tool-card')).toBeTruthy();
		expect(document.querySelector('.ask-waiting')).toBeNull();
		expect(screen.queryByText('方案 A')).toBeNull();
	});

	it('triggers onQuickReply with the message id and clicked option', async () => {
		const onQuickReply = vi.fn();
		render(
			ChatBubble,
			base({
				role: 'assistant',
				content: '选择？',
				type: 'ask',
				messageId: 'ask-42',
				awaiting: true,
				options: ['立即执行'],
				onQuickReply,
			}),
		);
		await fireEvent.click(screen.getByText('立即执行'));
		expect(onQuickReply).toHaveBeenCalledWith('ask-42', '立即执行');
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

describe('ChatBubble markdown code fences', () => {
	let clipboardMock;
	beforeEach(() => {
		clipboardMock = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText: clipboardMock }, configurable: true });
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	const renderMd = (content) => render(ChatBubble, { role: 'assistant', content, type: null, time: null });

	it('wraps a fenced code block with a toolbar and language label', async () => {
		const { container } = renderMd('```js\nconst a = 1;\n```');
		await waitFor(() => expect(container.querySelector('.md-code-wrap')).toBeTruthy());
		expect(container.querySelector('.md-code-lang').textContent).toBe('js');
		expect(container.querySelector('.md-code-copy')).toBeTruthy();
		expect(container.querySelector('.md-code-wrap code').textContent).toContain('const a = 1;');
	});

	it('labels unknown languages as text and still wraps the block', async () => {
		const { container } = renderMd('```\nplain lines\n```');
		await waitFor(() => expect(container.querySelector('.md-code-wrap')).toBeTruthy());
		expect(container.querySelector('.md-code-lang').textContent).toBe('text');
		expect(container.querySelector('.md-code-wrap code').textContent).toContain('plain lines');
	});

	it('copies the code text when the copy button is clicked', async () => {
		const { container } = renderMd('```python\nprint("hi")\n```');
		await waitFor(() => expect(container.querySelector('.md-code-copy')).toBeTruthy());
		await fireEvent.click(container.querySelector('.md-code-copy'));
		await waitFor(() => expect(clipboardMock).toHaveBeenCalled());
		expect(clipboardMock.mock.calls[0][0]).toContain('print("hi")');
	});

	it('flashes 已复制 on the button after a successful copy', async () => {
		const { container } = renderMd('```js\nx\n```');
		await waitFor(() => expect(container.querySelector('.md-code-copy')).toBeTruthy());
		const label = container.querySelector('.md-code-copy-text');
		vi.useFakeTimers();
		await fireEvent.click(container.querySelector('.md-code-copy'));
		await Promise.resolve();
		await Promise.resolve();
		expect(label.textContent).toBe('已复制');
		vi.runAllTimers();
		expect(label.textContent).toBe('复制');
	});

	it('leaves inline code untouched (no code wrap)', async () => {
		const { container } = renderMd('inline `code` here');
		await waitFor(() => expect(container.querySelector('.md-content')).toBeTruthy());
		await new Promise((r) => setTimeout(r, 0));
		expect(container.querySelector('.md-code-wrap')).toBeNull();
		expect(container.querySelector('.md-content code').textContent).toBe('code');
	});
});
