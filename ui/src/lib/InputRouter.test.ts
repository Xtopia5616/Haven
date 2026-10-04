import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/svelte';
import InputRouter from './InputRouter.svelte';
import inputRouterSource from './InputRouter.svelte?raw';
import GlobalContextMenu from './GlobalContextMenu.svelte';

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
		expect(inputRouterSource).toContain("icon={recordingState.isRecording ? 'pause' : 'mic'}");
		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toContain('录音');
	});

	it('keeps the recording shortcut in the active-session placeholder', () => {
		render(InputRouter, {
			activeSessionId: 'ses-1',
			hotkeyBinding: 'Alt+Space',
			onsubmit: vi.fn(),
		});

		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toBe('追加指令，Enter 发送，Shift+Enter 换行；按 Alt+Space 录音');
		expect(document.querySelector('.input-meta')).toBeNull();
	});

	it('moves ask guidance into the input placeholder', () => {
		render(InputRouter, { askAwaiting: true, askHasOptions: true, onsubmit: vi.fn() });

		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toBe('从上方的选项中选择，或者在此处输入答案或补充，Enter 提交');
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
		render(GlobalContextMenu);
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
		render(GlobalContextMenu);
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
		render(GlobalContextMenu);
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

describe('InputRouter attachment intake', () => {
	it('keeps a draft from submitting until selected attachments finish reading', async () => {
		const onsubmit = vi.fn();
		const readers: FileReader[] = [];
		const readAsDataURL = vi
			.spyOn(FileReader.prototype, 'readAsDataURL')
			.mockImplementation(function (this: FileReader) {
				readers.push(this);
			});

		try {
			const { container } = render(InputRouter, { onsubmit });
			const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
			const file = new File(['hello'], 'notes.txt', { type: 'text/plain' });
			await fireEvent.change(fileInput, { target: { files: [file] } });

			const textarea = screen.getByRole('textbox', { name: '消息输入框' });
			await fireEvent.input(textarea, { target: { value: 'include my attachment' } });
			const sendButton = screen.getByRole('button', { name: '发送' }) as HTMLButtonElement;
			expect(sendButton.disabled).toBe(true);

			await fireEvent.keyDown(textarea, { key: 'Enter' });
			expect(onsubmit).not.toHaveBeenCalled();
			expect((textarea as HTMLTextAreaElement).value).toBe('include my attachment');
			expect(readers).toHaveLength(1);

			Object.defineProperty(readers[0], 'result', {
				configurable: true,
				value: 'data:text/plain;base64,aGVsbG8=',
			});
			readers[0].dispatchEvent(new ProgressEvent('load'));
			await vi.waitFor(() => expect(sendButton.disabled).toBe(false));

			await fireEvent.click(sendButton);
			expect(onsubmit).toHaveBeenCalledWith({
				text: 'include my attachment',
				images: [],
				files: [
					{ media_type: 'text/plain', data: 'aGVsbG8=', filename: 'notes.txt', size: 5 },
				],
			});
		} finally {
			readAsDataURL.mockRestore();
		}
	});

	it('reserves file slots across overlapping reads', async () => {
		const onsubmit = vi.fn();
		const readers: FileReader[] = [];
		const readAsDataURL = vi
			.spyOn(FileReader.prototype, 'readAsDataURL')
			.mockImplementation(function (this: FileReader) {
				readers.push(this);
			});

		try {
			const { container } = render(InputRouter, { maxFiles: 1, onsubmit });
			const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
			await fireEvent.change(fileInput, {
				target: { files: [new File(['one'], 'one.txt', { type: 'text/plain' })] },
			});
			await fireEvent.change(fileInput, {
				target: { files: [new File(['two'], 'two.txt', { type: 'text/plain' })] },
			});
			expect(readers).toHaveLength(1);

			Object.defineProperty(readers[0], 'result', {
				configurable: true,
				value: 'data:text/plain;base64,b25l',
			});
			readers[0].dispatchEvent(new ProgressEvent('load'));
			await vi.waitFor(() => {
				expect(
					(screen.getByRole('button', { name: '发送' }) as HTMLButtonElement).disabled,
				).toBe(false);
			});

			await fireEvent.click(screen.getByRole('button', { name: '发送' }));
			expect(onsubmit).toHaveBeenCalledWith({
				text: '',
				images: [],
				files: [{ media_type: 'text/plain', data: 'b25l', filename: 'one.txt', size: 3 }],
			});
		} finally {
			readAsDataURL.mockRestore();
		}
	});
});

describe('InputRouter per-session drafts', () => {
	const onsubmit = vi.fn();

	it('isolates drafts between sessions and restores each draft when returning', async () => {
		const { container, rerender } = render(InputRouter, {
			activeSessionId: 'ses-a',
			onsubmit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		await fireEvent.input(textarea, { target: { value: 'draft A' } });
		await rerender({ activeSessionId: 'ses-b', onsubmit });
		expect(textarea.value).toBe('');

		await fireEvent.input(textarea, { target: { value: 'draft B' } });
		await rerender({ activeSessionId: 'ses-a', onsubmit });
		expect(textarea.value).toBe('draft A');

		await rerender({ activeSessionId: 'ses-b', onsubmit });
		expect(textarea.value).toBe('draft B');
	});

	it('keeps the fresh-session draft in its own slot', async () => {
		const { container, rerender } = render(InputRouter, { onsubmit });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		await fireEvent.input(textarea, { target: { value: 'new conversation draft' } });
		await rerender({ activeSessionId: 'ses-a', onsubmit });
		expect(textarea.value).toBe('');

		await fireEvent.input(textarea, { target: { value: 'session draft' } });
		await rerender({ activeSessionId: null, onsubmit });
		expect(textarea.value).toBe('new conversation draft');

		await rerender({ activeSessionId: 'ses-a', onsubmit });
		expect(textarea.value).toBe('session draft');
	});

	it('clears the submitted draft instead of restoring it on the next visit', async () => {
		const submit = vi.fn();
		const { container, rerender } = render(InputRouter, {
			activeSessionId: 'ses-a',
			onsubmit: submit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.input(textarea, { target: { value: 'send once' } });
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(submit).toHaveBeenCalledWith({ text: 'send once', images: [], files: [] });
		expect(textarea.value).toBe('');
		await rerender({ activeSessionId: 'ses-b', onsubmit: submit });
		await rerender({ activeSessionId: 'ses-a', onsubmit: submit });
		expect(textarea.value).toBe('');
	});

	it('evicts the oldest cached draft after the 100-session limit', async () => {
		const { container, rerender } = render(InputRouter, {
			activeSessionId: 'ses-0',
			onsubmit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		for (let index = 0; index <= 101; index += 1) {
			if (index > 0) {
				await rerender({ activeSessionId: `ses-${index}`, onsubmit });
			}
			await fireEvent.input(textarea, { target: { value: `draft-${index}` } });
		}

		await rerender({ activeSessionId: 'ses-0', onsubmit });
		expect(textarea.value).toBe('');

		await rerender({ activeSessionId: 'ses-2', onsubmit });
		expect(textarea.value).toBe('draft-2');
	});
});
