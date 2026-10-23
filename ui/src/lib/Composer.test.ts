import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, within } from '@testing-library/svelte';
import Composer from './Composer.svelte';
import composerSource from './Composer.svelte?raw';
import GlobalContextMenu from './GlobalContextMenu.svelte';

describe('Composer context menu', () => {
	beforeEach(() => {
		Object.defineProperty(navigator, 'clipboard', {
			value: { writeText: vi.fn().mockResolvedValue(undefined), readText: vi.fn() },
			configurable: true,
		});
	});

	it('submits the selected delivery mode while work is running', async () => {
		const onsubmit = vi.fn();
		render(Composer, { sessionRunning: true, onsubmit });
		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		expect(
			within(deliveryMode)
				.getByRole('button', { name: '安全边界调整' })
				.getAttribute('aria-pressed'),
		).toBe('true');
		await fireEvent.click(within(deliveryMode).getByRole('button', { name: '完成后追加' }));
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'later' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));
		expect(onsubmit).toHaveBeenCalledWith(
			expect.objectContaining({ text: 'later', deliveryMode: 'follow_up' }),
		);
	});

	it('defaults to steering while work is active and includes it in submission', async () => {
		const onsubmit = vi.fn();
		render(Composer, { sessionRunning: true, onsubmit });

		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		expect(
			within(deliveryMode)
				.getByRole('button', { name: '安全边界调整' })
				.getAttribute('aria-pressed'),
		).toBe('true');
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'change direction' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(onsubmit).toHaveBeenCalledWith(
			expect.objectContaining({ text: 'change direction', deliveryMode: 'steering' }),
		);
	});

	it('keeps the delivery mode available during active generation before status refresh', async () => {
		const onsubmit = vi.fn();
		render(Composer, { isGenerating: true, onsubmit });

		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		await fireEvent.click(within(deliveryMode).getByRole('button', { name: '完成后追加' }));
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'wait for natural completion' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(onsubmit).toHaveBeenCalledWith(
			expect.objectContaining({ deliveryMode: 'follow_up' }),
		);
	});

	it('disables steering and follow-up while a delivery is pending', async () => {
		const onsubmit = vi.fn();
		render(Composer, { sessionRunning: true, hasPendingDelivery: true, onsubmit });
		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		const steering = within(deliveryMode).getByRole('button', { name: '安全边界调整' });
		const followUp = within(deliveryMode).getByRole('button', { name: '完成后追加' });
		expect(steering).toHaveProperty('disabled', true);
		expect(followUp).toHaveProperty('disabled', true);

		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'another message' },
		});
		expect(screen.getByRole('button', { name: '发送' })).toHaveProperty('disabled', true);
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));
		expect(onsubmit).not.toHaveBeenCalled();
	});

	it('hides delivery modes and blocks composer sends while interactions are pending', async () => {
		const onsubmit = vi.fn();
		render(Composer, {
			sessionRunning: true,
			hasPendingInteractions: true,
			onsubmit,
		});
		expect(screen.queryByRole('group', { name: '消息投递方式' })).toBeNull();

		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'wait until pending interactions are resolved' },
		});
		expect(screen.getByRole('button', { name: '发送' })).toHaveProperty('disabled', true);
		expect(onsubmit).not.toHaveBeenCalled();
	});

	it('keeps queue timing guidance out of the composer', () => {
		render(Composer, {
			sessionRunning: true,
			sessionStatus: 'pending',
			executionPhase: 'queued',
			onsubmit: vi.fn(),
		});
		expect(screen.queryByText('会话排队中；本条消息会在会话启动后的安全边界接入。')).toBeNull();
	});

	it('routes all composer input to the pending Ask without a delivery selector', async () => {
		const submit = vi.fn();
		render(Composer, {
			askAwaiting: true,
			askHasOptions: true,
			sessionRunning: true,
			hasPendingInteractions: true,
			onsubmit: submit,
		});

		expect(screen.queryByRole('combobox', { name: '问题回复方式' })).toBeNull();
		expect(screen.queryByRole('group', { name: '消息投递方式' })).toBeNull();
		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toContain('输入答案或补充');
		expect(
			screen.queryByText('正在等待你的回答；发送内容和已选选项会作为当前问题的答案。'),
		).toBeNull();
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'the answer' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(submit).toHaveBeenCalledWith({ text: 'the answer', images: [], files: [] });
	});

	it('keeps selected Ask chips submit-ready without a follow-up mode', async () => {
		const onsubmit = vi.fn();
		render(Composer, {
			askAwaiting: true,
			allowEmptySubmit: true,
			hasPendingInteractions: true,
			onsubmit,
		});

		expect(screen.getByRole('button', { name: '发送' })).toHaveProperty('disabled', false);
		expect(screen.queryByRole('combobox', { name: '问题回复方式' })).toBeNull();
	});

	it('submits selected Ask chips without a delivery override', async () => {
		const onsubmit = vi.fn();
		render(Composer, {
			askAwaiting: true,
			allowEmptySubmit: true,
			hasPendingInteractions: true,
			onsubmit,
		});

		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(onsubmit).toHaveBeenCalledWith({ text: '', images: [], files: [] });
	});

	it('keeps tool-boundary guidance out of the composer', async () => {
		render(Composer, {
			sessionRunning: true,
			executionPhase: 'waiting_result',
			onsubmit: vi.fn(),
		});

		expect(
			screen.queryByText('工具仍在执行；安全边界调整会等工具返回后接入，不会中断工具。'),
		).toBeNull();
		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		await fireEvent.click(within(deliveryMode).getByRole('button', { name: '完成后追加' }));
		expect(
			screen.queryByText('工具仍在执行；完成后追加会等整个工作自然结束，不会中断工具。'),
		).toBeNull();
	});

	it('queues paused submissions and blocks ordinary input during confirmation', async () => {
		const pausedSubmit = vi.fn();
		const paused = render(Composer, {
			activeSessionId: 'ses-00000000000000000000000000000001',
			sessionStatus: 'paused',
			sessionWaitingReason: 'step_budget',
			onsubmit: pausedSubmit,
		});
		expect(
			screen.queryByText('本轮步骤预算已耗尽；发送普通消息会排入后续并恢复会话。'),
		).toBeNull();
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'resume with this' },
		});
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));
		expect(pausedSubmit).toHaveBeenCalledWith(
			expect.objectContaining({ text: 'resume with this', deliveryMode: 'follow_up' }),
		);
		paused.unmount();

		const confirmationSubmit = vi.fn();
		render(Composer, {
			activeSessionId: 'ses-00000000000000000000000000000001',
			confirmationAwaiting: true,
			onsubmit: confirmationSubmit,
		});
		expect(
			screen.queryByText(
				'工具正在等待权限确认；此处发送的消息只会排队，确认前不会开始处理。',
			),
		).toBeNull();
		expect(screen.queryByRole('group', { name: '消息投递方式' })).toBeNull();
		await fireEvent.input(screen.getByRole('textbox', { name: '消息输入框' }), {
			target: { value: 'after confirmation' },
		});
		expect(screen.getByRole('button', { name: '发送' })).toHaveProperty('disabled', true);
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));
		expect(confirmationSubmit).not.toHaveBeenCalled();
	});

	it('shows a completed turn as the next-turn boundary', () => {
		render(Composer, {
			activeSessionId: 'ses-00000000000000000000000000000001',
			sessionStatus: 'paused',
			sessionStatusLabel: '本轮已完成',
			onsubmit: vi.fn(),
		});
		expect(
			screen.queryByText('当前工作已自然完成；发送消息会作为下一轮追加并继续会话。'),
		).toBeNull();
		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toContain('追加指令');
	});

	it('shows stop progress and disables delivery changes while interruption is pending', () => {
		render(Composer, {
			sessionRunning: true,
			isGenerating: true,
			interrupting: true,
			onsubmit: vi.fn(),
		});
		expect(screen.queryByText('正在停止当前输出；停止完成后再发送新消息。')).toBeNull();
		const deliveryMode = screen.getByRole('group', { name: '消息投递方式' });
		expect(within(deliveryMode).getByRole('button', { name: '安全边界调整' })).toHaveProperty(
			'disabled',
			true,
		);
		expect(within(deliveryMode).getByRole('button', { name: '完成后追加' })).toHaveProperty(
			'disabled',
			true,
		);
		expect(screen.getByRole('button', { name: '中断输出' })).toHaveProperty('disabled', true);
	});

	it.each(['completed', 'error'] as const)(
		'keeps the %s state guidance out of the composer',
		(sessionStatus) => {
			render(Composer, {
				activeSessionId: 'ses-00000000000000000000000000000001',
				sessionStatus,
				onsubmit: vi.fn(),
			});
			expect(screen.queryByText('此会话已结束；发送新消息会开始新的会话。')).toBeNull();
		},
	);

	it('uses the shared toolbar control for attachment, recording, and send actions', () => {
		render(Composer, { onsubmit: vi.fn() });

		for (const label of ['添加附件', '开始录音', '发送']) {
			const button = screen.getByRole('button', { name: label });
			expect(button.classList.contains('md-icon-btn')).toBe(true);
			expect(button.getAttribute('data-size')).toBe('toolbar');
		}
		expect(screen.getByRole('button', { name: '发送' }).getAttribute('data-variant')).toBe(
			'primary',
		);
		expect(composerSource).toContain("icon={recordingState.isRecording ? 'pause' : 'mic'}");
		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toContain('录音');
	});

	it('keeps the recording shortcut in the active-session placeholder', () => {
		render(Composer, {
			activeSessionId: 'ses-00000000000000000000000000000001',
			hotkeyBinding: 'Alt+Space',
			onsubmit: vi.fn(),
		});

		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toBe('追加指令，Enter 发送，Shift+Enter 换行；按 Alt+Space 录音');
		expect(document.querySelector('.input-meta')).toBeNull();
	});

	it('moves ask guidance into the input placeholder', () => {
		render(Composer, { askAwaiting: true, askHasOptions: true, onsubmit: vi.fn() });

		expect(
			screen.getByRole('textbox', { name: '消息输入框' }).getAttribute('placeholder'),
		).toBe('从上方的选项中选择，或者在此处输入答案或补充，Enter 提交');
	});

	it('uses the same shared toolbar control for interrupting active output', () => {
		const onstop = vi.fn();
		render(Composer, { isGenerating: true, onstop });

		const button = screen.getByRole('button', { name: '中断输出' });
		expect(button.classList.contains('md-icon-btn')).toBe(true);
		expect(button.getAttribute('data-size')).toBe('toolbar');
		expect(button.getAttribute('data-variant')).toBe('danger');

		fireEvent.click(button);
		expect(onstop).toHaveBeenCalledTimes(1);
	});

	it('keeps the interrupt label stable while the stop request is in flight', () => {
		render(Composer, { isGenerating: true, interrupting: true });

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
		const { container } = render(Composer, { onsubmit: vi.fn() });
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
		const { container } = render(Composer, { onsubmit: vi.fn() });
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
		const { container } = render(Composer, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.input(textarea, { target: { value: 'drop me' } });
		await fireEvent.contextMenu(textarea);
		await fireEvent.click(screen.getByText('清空'));
		expect(textarea.value).toBe('');
	});

	it('balances a single-line draft with the computed line height', async () => {
		const { container } = render(Composer, { onsubmit: vi.fn() });
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

	it('uses the compact CSS minimum when growing a draft in a short window', async () => {
		const { container } = render(Composer, { onsubmit: vi.fn() });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		Object.defineProperty(textarea, 'scrollHeight', { configurable: true, value: 38 });
		Object.defineProperty(textarea, 'clientHeight', { configurable: true, value: 40 });
		const computedStyle = vi.spyOn(window, 'getComputedStyle').mockReturnValue({
			minHeight: '40px',
			lineHeight: '20px',
			borderTopWidth: '0px',
			borderBottomWidth: '0px',
		} as CSSStyleDeclaration);
		try {
			await fireEvent.input(textarea, { target: { value: '窄屏草稿' } });
			expect(textarea.style.height).toBe('40px');
			expect(textarea.style.paddingTop).toBe('10px');
		} finally {
			computedStyle.mockRestore();
		}
	});

	it('keeps the chat draft text horizontally left-aligned', () => {
		expect(composerSource).toMatch(/\.chat-input\s*\{[\s\S]*text-align:\s*left;/);
	});
});

describe('Composer attachment intake', () => {
	it('keeps same-name file preview identity stable and omits local keys on submit', async () => {
		const onsubmit = vi.fn();
		const readers: FileReader[] = [];
		const readAsDataURL = vi
			.spyOn(FileReader.prototype, 'readAsDataURL')
			.mockImplementation(function (this: FileReader) {
				readers.push(this);
			});

		try {
			const { container } = render(Composer, { onsubmit });
			const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
			await fireEvent.change(fileInput, {
				target: {
					files: [
						new File(['first'], 'same.txt', { type: 'text/plain' }),
						new File(['second'], 'same.txt', { type: 'text/plain' }),
					],
				},
			});
			await vi.waitFor(() => expect(readers).toHaveLength(1));

			Object.defineProperty(readers[0], 'result', {
				configurable: true,
				value: 'data:text/plain;base64,Zmlyc3Q=',
			});
			readers[0].dispatchEvent(new ProgressEvent('load'));
			await vi.waitFor(() => expect(readers).toHaveLength(2));

			Object.defineProperty(readers[1], 'result', {
				configurable: true,
				value: 'data:text/plain;base64,c2Vjb25k',
			});
			readers[1].dispatchEvent(new ProgressEvent('load'));
			await vi.waitFor(() =>
				expect(container.querySelectorAll('.file-preview')).toHaveLength(2),
			);

			const remainingPreview = container.querySelectorAll('.file-preview')[1];
			await fireEvent.click(screen.getAllByRole('button', { name: /移除附件 same\.txt/ })[0]);
			expect(container.querySelector('.file-preview')).toBe(remainingPreview);

			await fireEvent.click(screen.getByRole('button', { name: '发送' }));
			expect(onsubmit).toHaveBeenCalledWith({
				text: '',
				images: [],
				files: [{ media_type: 'text/plain', data: 'c2Vjb25k', filename: 'same.txt' }],
			});
		} finally {
			readAsDataURL.mockRestore();
		}
	});

	it('keeps identical image preview identity stable and omits local keys on submit', async () => {
		const onsubmit = vi.fn();
		const readers: FileReader[] = [];
		const readAsDataURL = vi
			.spyOn(FileReader.prototype, 'readAsDataURL')
			.mockImplementation(function (this: FileReader) {
				readers.push(this);
			});

		try {
			const { container } = render(Composer, { onsubmit });
			const fileInput = container.querySelector('input[type="file"]') as HTMLInputElement;
			await fireEvent.change(fileInput, {
				target: {
					files: [
						new File(['same image'], 'first.png', { type: 'image/png' }),
						new File(['same image'], 'second.png', { type: 'image/png' }),
					],
				},
			});
			await vi.waitFor(() => expect(readers).toHaveLength(1));

			for (let index = 0; index < 2; index += 1) {
				Object.defineProperty(readers[index], 'result', {
					configurable: true,
					value: 'data:image/png;base64,c2FtZS1pbWFnZQ==',
				});
				readers[index].dispatchEvent(new ProgressEvent('load'));
				if (index === 0) await vi.waitFor(() => expect(readers).toHaveLength(2));
			}
			await vi.waitFor(() =>
				expect(container.querySelectorAll('.image-preview')).toHaveLength(2),
			);

			const remainingPreview = container.querySelectorAll('.image-preview')[1];
			await fireEvent.click(screen.getByRole('button', { name: '移除第 1 张待发送图片' }));
			expect(container.querySelector('.image-preview')).toBe(remainingPreview);

			await fireEvent.click(screen.getByRole('button', { name: '发送' }));
			expect(onsubmit).toHaveBeenCalledWith({
				text: '',
				images: [{ media_type: 'image/png', data: 'c2FtZS1pbWFnZQ==' }],
				files: [],
			});
		} finally {
			readAsDataURL.mockRestore();
		}
	});

	it('keeps a draft from submitting until selected attachments finish reading', async () => {
		const onsubmit = vi.fn();
		const readers: FileReader[] = [];
		const readAsDataURL = vi
			.spyOn(FileReader.prototype, 'readAsDataURL')
			.mockImplementation(function (this: FileReader) {
				readers.push(this);
			});

		try {
			const { container } = render(Composer, { onsubmit });
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
			expect(screen.getByText('5 B')).toBeTruthy();

			await fireEvent.click(sendButton);
			expect(onsubmit).toHaveBeenCalledWith({
				text: 'include my attachment',
				images: [],
				files: [{ media_type: 'text/plain', data: 'aGVsbG8=', filename: 'notes.txt' }],
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
			const { container } = render(Composer, { maxFiles: 1, onsubmit });
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
				files: [{ media_type: 'text/plain', data: 'b25l', filename: 'one.txt' }],
			});
		} finally {
			readAsDataURL.mockRestore();
		}
	});
});

describe('Composer per-session drafts', () => {
	const onsubmit = vi.fn();

	it('isolates drafts between sessions and restores each draft when returning', async () => {
		const { container, rerender } = render(Composer, {
			activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			onsubmit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		await fireEvent.input(textarea, { target: { value: 'draft A' } });
		await rerender({ activeSessionId: 'ses-b77487af9bb14e5ec4d60572f1248bce', onsubmit });
		expect(textarea.value).toBe('');

		await fireEvent.input(textarea, { target: { value: 'draft B' } });
		await rerender({ activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074', onsubmit });
		expect(textarea.value).toBe('draft A');

		await rerender({ activeSessionId: 'ses-b77487af9bb14e5ec4d60572f1248bce', onsubmit });
		expect(textarea.value).toBe('draft B');
	});

	it('keeps the fresh-session draft in its own slot', async () => {
		const { container, rerender } = render(Composer, { onsubmit });
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		await fireEvent.input(textarea, { target: { value: 'new conversation draft' } });
		await rerender({ activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074', onsubmit });
		expect(textarea.value).toBe('');

		await fireEvent.input(textarea, { target: { value: 'session draft' } });
		await rerender({ activeSessionId: null, onsubmit });
		expect(textarea.value).toBe('new conversation draft');

		await rerender({ activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074', onsubmit });
		expect(textarea.value).toBe('session draft');
	});

	it('clears the submitted draft instead of restoring it on the next visit', async () => {
		const submit = vi.fn();
		const { container, rerender } = render(Composer, {
			activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			onsubmit: submit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
		await fireEvent.input(textarea, { target: { value: 'send once' } });
		await fireEvent.click(screen.getByRole('button', { name: '发送' }));

		expect(submit).toHaveBeenCalledWith({ text: 'send once', images: [], files: [] });
		expect(textarea.value).toBe('');
		await rerender({
			activeSessionId: 'ses-b77487af9bb14e5ec4d60572f1248bce',
			onsubmit: submit,
		});
		await rerender({
			activeSessionId: 'ses-32f23b1e00f85571a60f75dde5588074',
			onsubmit: submit,
		});
		expect(textarea.value).toBe('');
	});

	it('evicts the oldest cached draft after the 100-session limit', async () => {
		const { container, rerender } = render(Composer, {
			activeSessionId: 'ses-00000000000000000000000000000000',
			onsubmit,
		});
		const textarea = container.querySelector('textarea') as HTMLTextAreaElement;

		for (let index = 0; index <= 101; index += 1) {
			if (index > 0) {
				await rerender({
					activeSessionId: `ses-${index.toString(16).padStart(32, '0')}`,
					onsubmit,
				});
			}
			await fireEvent.input(textarea, { target: { value: `draft-${index}` } });
		}

		await rerender({ activeSessionId: 'ses-00000000000000000000000000000000', onsubmit });
		expect(textarea.value).toBe('');

		await rerender({ activeSessionId: 'ses-00000000000000000000000000000002', onsubmit });
		expect(textarea.value).toBe('draft-2');
	});
});

describe('Composer imperative input API', () => {
	it('sets the draft for rollback restoration', async () => {
		const { component, container } = render(Composer, { onsubmit: vi.fn() });
		const input = container.querySelector('textarea') as HTMLTextAreaElement;

		(component as { setDraft: (text: string) => void }).setDraft('恢复后的消息');

		await vi.waitFor(() => expect(input.value).toBe('恢复后的消息'));
	});
});
