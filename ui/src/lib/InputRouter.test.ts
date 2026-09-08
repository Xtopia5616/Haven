import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import InputRouter from './InputRouter.svelte';
import inputRouterSource from './InputRouter.svelte?raw';

describe('InputRouter context menu', () => {
	beforeEach(() => {
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: vi.fn().mockResolvedValue(undefined), readText: vi.fn() },
			configurable: true,
		});
	});

	it('uses the shared toolbar control for attachment, recording, and send actions', () => {
		render(InputRouter, { onsubmit: vi.fn() });

		for (const label of ['添加附件', '开始录音', '发送']) {
			const button = screen.getByRole('button', { name: label });
			expect(button.classList.contains('md-icon-btn')).toBe(true);
			expect(button.getAttribute('data-size')).toBe('toolbar');
		}
		expect(screen.getByRole('button', { name: '发送' }).getAttribute('data-variant')).toBe(
			'primary',
		);
		expect(screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder')).toContain(
			'录音',
		);
	});

	it('keeps the recording shortcut in the active-session placeholder', () => {
		render(InputRouter, { activeSessionId: 'ses-1', hotkeyBinding: 'Alt+Space', onsubmit: vi.fn() });

		expect(screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder')).toBe(
			'追加指令，Enter 发送，Shift+Enter 换行；按 Alt+Space 录音',
		);
		expect(document.querySelector('.input-meta')).toBeNull();
	});

	it('uses the same shared toolbar control for interrupting active output', () => {
		const onstop = vi.fn();
		render(InputRouter, { isGenerating: true, onstop });

		const button = screen.getByRole('button', { name: '中断输出' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');
		expect(button.getAttribute('data-variant')).toBe('danger');

		fireEvent.click(button);
		expect(onstop).toHaveBeenCalledTimes(1);
	});

	it('keeps the interrupt label stable while the stop request is in flight', () => {
		render(InputRouter, { isGenerating: true, interrupting: true });

		const button = screen.getByRole('button', { name: '中断输出' });
		expect((button as HTMLButtonElement).disabled).toBe(true);
		expect(button.getAttribute('aria-busy')).toBe('true');
	});

	it('opens a copy menu on the input and copies the draft', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText, readText: vi.fn() },
			configurable: true,
		});
		const { container } = render(InputRouter, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.input(textarea, { target: { value: 'hello haven' } });
		await fireEvent.contextMenu(textarea, { clientX: 12, clientY: 24 });
		expect(screen.getByText('复制')).toBeTruthy();
		expect(screen.getByText('粘贴')).toBeTruthy();
		expect(screen.getByText('清空')).toBeTruthy();
		await fireEvent.click(screen.getByText('复制'));
		expect(writeText).toHaveBeenCalledWith('hello haven');
	});

	it('pastes clipboard text into the draft', async () => {
		const readText = vi.fn().mockResolvedValue('pasted');
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: vi.fn(), readText },
			configurable: true,
		});
		const { container } = render(InputRouter, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.contextMenu(textarea);
		await fireEvent.click(screen.getByText('粘贴'));
		expect(readText).toHaveBeenCalled();
		await vi.waitFor(() => {
			expect(textarea.value).toBe('pasted');
		});
	});

	it('clears the draft from the menu', async () => {
		const { container } = render(InputRouter, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.input(textarea, { target: { value: 'drop me' } });
		await fireEvent.contextMenu(textarea);
		await fireEvent.click(screen.getByText('清空'));
		expect(textarea.value).toBe('');
	});

	it('balances a single-line draft with the computed line height', async () => {
		const { container } = render(InputRouter, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		Object.defineProperty(textarea, 'scrollHeight', { configurable: true, value: 48 });
		Object.defineProperty(textarea, 'clientHeight', { configurable: true, value: 48 });
		const getComputedStyleSpy = vi.spyOn(window, 'getComputedStyle').mockReturnValue({
			lineHeight: '30px',
			borderTopWidth: '1px',
			borderBottomWidth: '1px',
		} as CSSStyleDeclaration);

		try {
			await fireEvent.input(textarea, { target: { value: 'center me' } });
			expect(textarea.style.paddingTop).toBe('8px');
			expect(textarea.style.paddingBottom).toBe('8px');
		} finally {
			getComputedStyleSpy.mockRestore();
		}
	});

	it('keeps the chat draft text horizontally left-aligned', () => {
		expect(inputRouterSource).toMatch(/\.chat-input\s*\{[\s\S]*text-align:\s*left;/);
	});
});
