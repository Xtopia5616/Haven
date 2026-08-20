<script>
	import logger from '$lib/logger.ts';
	import { browser } from '$app/environment';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification, recordingOverlay, imageDataUrl } from '$lib/stores.ts';
	import { formatError } from '$lib/formatError.ts';
	import { syncStore } from '$lib/syncStore.ts';

	let {
		activeSessionId = null,
		hotkeyBinding = 'Ctrl+Shift+Space',
		isGenerating = false,
		sessionRunning = false,
		onsubmit,
		onstop,
		toolbarLeft,
		toolbarRight,
		// Attachment & compression limits, driven by the settings "输入"
		// page via [context_limits]; defaults mirror the backend config.
		maxImages = 4,
		maxImageBytes = 10 * 1024 * 1024,
		maxImageDim = 1568,
		jpegQuality = 0.85,
		maxFiles = 5,
		maxFileBytes = 20 * 1024 * 1024,
	} = $props();

	// Pending image attachments (multimodal): [{ mediaType, data }] with data
	// holding base64 bytes (no data: prefix). Filled by paste / file picker,
	// sent along with the next message, cleared on submit.
	/** @type {any[]} */
	let pendingImages = $state([]);

	// Pending non-image file attachments: [{ media_type, data, filename, size }].
	// Read as base64 when picked, persisted by the backend to disk and handed
	// to the agent as a path the file tool can read.
	/** @type {any[]} */
	let pendingFiles = $state([]);
	// Single hidden picker for both images and files; the picked items are
	// split by type on selection (images -> pendingImages, rest -> pendingFiles).
	let attachFileInput = /** @type {HTMLInputElement | null} */ ($state(null));

	// Recording state (mirror of the global recordingOverlay store) so the
	// toolbar mic button can toggle start/stop inline.
	let recordingState = $state({ isRecording: false });
	$effect(() =>
		syncStore(recordingOverlay, (v) => {
			recordingState = v;
		}),
	);

	let transcriptInput = $state('');
	let transcriptTextarea = /** @type {HTMLTextAreaElement | null} */ ($state(null));

	const hasInput = $derived(
		transcriptInput.trim().length > 0 || pendingImages.length > 0 || pendingFiles.length > 0,
	);
	// While the agent is generating, a sent message is delivered immediately
	// to the backend: the agent injects it in the gap between tool calls and
	// the final content, so it can steer the answer instead of waiting for
	// the whole turn to finish.
	// The merged send button becomes "stop session" only when there is no input
	// and the agent is actively working (generating output, a running/pending
	// session). With fresh input present, it always stays a send button.
	const stopMode = $derived(!hasInput && (isGenerating || sessionRunning));

	// Allow the host page to populate the draft box programmatically (e.g.
	// restoring a message after rollback) via `bind:this`.
	/** @param {string} text */
	export function setDraft(text) {
		transcriptInput = text ?? '';
	}

	async function handleRecordClick() {
		try {
			if (recordingState.isRecording) {
				// Optimistic stop: flip the overlay instantly; the backend
				// confirms via recording:stopped ~50 ms later.
				recordingOverlay.update((v) => ({ ...v, isRecording: false, visible: false }));
				try {
					await invoke('stop_recording');
				} catch (e) {
					recordingOverlay.update((v) => ({ ...v, isRecording: true, visible: true }));
					throw e;
				}
			} else {
				// Optimistic start: the button/overlay respond immediately so
				// the brief stream-startup wait (~90 ms) behind `start_recording`
				// is not perceived as a laggy click.
				recordingOverlay.update((v) => ({ ...v, isRecording: true, visible: true }));
				try {
					await invoke('start_recording');
				} catch (e) {
					recordingOverlay.update((v) => ({ ...v, isRecording: false, visible: false }));
					// The backend already emits `recording:error` with a
					// friendly message (surfaced as a notification by the
					// layout), so do not re-throw — that would show a second,
					// redundant error toast.
				}
			}
		} catch (e) {
			addNotification(`录音失败: ${formatError(e)}`, 'error', 3000);
		}
	}

	/** Read a File as a { media_type, data } attachment without re-encoding. */
	/** @param {File} file */
	function readAsAttachment(file) {
		return new Promise((resolve, reject) => {
			const reader = new FileReader();
			reader.onload = () => {
				const dataUrl = String(reader.result || '');
				const comma = dataUrl.indexOf(',');
				const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
				resolve({ media_type: file.type || 'application/octet-stream', data: base64 });
			};
			reader.onerror = () => reject(new Error('文件读取失败'));
			reader.readAsDataURL(file);
		});
	}

	/**
	 * Downscale and re-encode an image File to JPEG to reduce payload size.
	 * Returns null if compression isn't possible (e.g. browser lacks the API).
	 */
	/** @param {File} file */
	async function tryCompressImage(file) {
		if (typeof createImageBitmap !== 'function' || typeof document === 'undefined') return null;
		try {
			const bitmap = await createImageBitmap(file);
			let { width, height } = bitmap;
			const maxDim = Math.max(width, height);
			if (maxDim > maxImageDim) {
				const scale = maxImageDim / maxDim;
				width = Math.round(width * scale);
				height = Math.round(height * scale);
			}
			const canvas = document.createElement('canvas');
			canvas.width = width;
			canvas.height = height;
			const ctx = canvas.getContext('2d');
			if (!ctx) return null;
			ctx.drawImage(bitmap, 0, 0, width, height);
			bitmap.close?.();
			const dataUrl = canvas.toDataURL('image/jpeg', jpegQuality);
			const comma = dataUrl.indexOf(',');
			return {
				media_type: 'image/jpeg',
				data: comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl,
			};
		} catch (e) {
			logger.warn('InputRouter', 'image compression failed, using original', e);
			return null;
		}
	}

	/**
	 * Convert a File to a { media_type, data } attachment (base64, no prefix).
	 * Compresses to JPEG when the result is smaller than the original;
	 * otherwise keeps the original encoding.
	 */
	/** @param {File} file */
	async function fileToAttachment(file) {
		if (file.size > maxImageBytes) {
			throw new Error(`图片超过 ${Math.round(maxImageBytes / 1024 / 1024)}MB 上限`);
		}
		const original = await readAsAttachment(file);
		const compressed = await tryCompressImage(file);
		if (compressed && compressed.data.length < original.data.length) {
			return compressed;
		}
		return original;
	}

	const IMAGE_EXTENSIONS = new Set([
		'png',
		'jpg',
		'jpeg',
		'gif',
		'webp',
		'bmp',
		'svg',
		'avif',
		'ico',
	]);

	/**
	 * Decide whether a picked file counts as an image (vision path) or a
	 * generic file (disk path) by MIME type first, then extension — so a
	 * `.png` with a missing/odd MIME still routes to the image logic.
	 */
	/** @param {File} file */
	function isImageFile(file) {
		if (file.type && file.type.startsWith('image/')) return true;
		const ext = (file.name.split('.').pop() || '').toLowerCase();
		return IMAGE_EXTENSIONS.has(ext);
	}

	/** @param {FileList | File[]} files */
	async function addPendingImages(files) {
		if (!files || files.length === 0) return;
		const room = maxImages - pendingImages.length;
		if (room <= 0) {
			addNotification(`最多支持 ${maxImages} 张图片`, 'error', 3000);
			return;
		}
		const list = Array.from(files).slice(0, room);
		for (const f of list) {
			if (!isImageFile(f)) {
				addNotification(`不支持的文件类型: ${f.name}`, 'error', 3000);
				continue;
			}
			try {
				pendingImages = [...pendingImages, await fileToAttachment(f)];
			} catch (e) {
				addNotification(formatError(e) || '图片读取失败', 'error', 3000);
			}
		}
	}

	/** @param {ClipboardEvent} e */
	function handlePaste(e) {
		const items = e.clipboardData?.items;
		if (!items) return;
		const images = [];
		for (const item of items) {
			if (item.type.startsWith('image/')) {
				const file = item.getAsFile();
				if (file) images.push(file);
			}
		}
		if (images.length > 0) {
			e.preventDefault();
			addPendingImages(images);
		}
	}

	/** @param {number} index */
	function removePendingImage(index) {
		pendingImages = pendingImages.filter((_, i) => i !== index);
	}

	/** @param {number} bytes */
	function formatFileSize(bytes) {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
	}

	// Read non-image files as base64 attachments (with the original name) so
	// the backend can persist them to disk and hand the agent a path. Files
	// are capped at maxFiles / maxFileBytes, mirroring server validation.
	/** @param {FileList | File[]} files */
	async function addPendingFiles(files) {
		if (!files || files.length === 0) return;
		const room = maxFiles - pendingFiles.length;
		if (room <= 0) {
			addNotification(`最多支持 ${maxFiles} 个文件`, 'error', 3000);
			return;
		}
		const list = Array.from(files).slice(0, room);
		for (const f of list) {
			if (f.size > maxFileBytes) {
				addNotification(
					`文件超过 ${Math.round(maxFileBytes / 1024 / 1024)}MB 上限: ${f.name}`,
					'error',
					3000,
				);
				continue;
			}
			try {
				const { media_type, data } = await readAsAttachment(f);
				pendingFiles = [
					...pendingFiles,
					{ media_type, data, filename: f.name, size: f.size },
				];
			} catch (e) {
				addNotification(formatError(e) || '文件读取失败', 'error', 3000);
			}
		}
	}

	// Single entry point for the attachment picker: images (by MIME/extension)
	// go to the vision preview row, everything else to the file chips.
	/** @param {any} e */
	function handleAttachSelect(e) {
		const files = Array.from(e.target.files || []);
		const images = files.filter(isImageFile);
		const others = files.filter((f) => !isImageFile(f));
		if (images.length > 0) addPendingImages(images);
		if (others.length > 0) addPendingFiles(others);
		e.target.value = '';
	}

	/** @param {number} index */
	function removePendingFile(index) {
		pendingFiles = pendingFiles.filter((_, i) => i !== index);
	}

	// Collect whatever is currently pending (text, images, files) into a
	// single normalized payload and forward it to the host, then clear the
	// draft. The host owns the actual submission side effects.
	function handleSubmit() {
		const text = transcriptInput.trim();
		const images = pendingImages;
		const files = pendingFiles;
		if (!text && images.length === 0 && files.length === 0) return;
		transcriptInput = '';
		pendingImages = [];
		pendingFiles = [];
		onsubmit?.({ text, images, files });
	}

	/** @param {KeyboardEvent} e */
	function handleKeydown(e) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
			e.preventDefault();
			handleSubmit();
		}
	}

	// Auto-grow the input to fit its content. While the content is a single
	// line, the vertical padding is balanced so the text renders centered
	// (matching the placeholder); multi-line content uses a fixed padding.
	const CHAT_INPUT_MIN_H = 44;
	const CHAT_INPUT_BASE_PAD = 8;
	const CHAT_INPUT_LINE_H = 20.3; // 14px font-size × 1.45 line-height
	function autoGrowInput() {
		const el = transcriptTextarea;
		if (!el) return;
		el.style.height = 'auto';
		el.style.paddingTop = '';
		el.style.paddingBottom = '';
		const contentH = el.scrollHeight;
		const singleLine = contentH <= CHAT_INPUT_MIN_H;
		el.style.height = Math.max(CHAT_INPUT_MIN_H, contentH) + 'px';
		if (singleLine) {
			// Balance the vertical padding against the inner height (border
			// excluded) so the single line of text sits exactly centered.
			const innerH = el.clientHeight;
			const totalPad = Math.max(0, innerH - CHAT_INPUT_LINE_H);
			const pad = Math.floor(totalPad / 2);
			el.style.paddingTop = pad + 'px';
			el.style.paddingBottom = totalPad - pad + 'px';
			el.style.setProperty('--chat-pad', pad + 'px');
		} else {
			el.style.setProperty('--chat-pad', CHAT_INPUT_BASE_PAD + 'px');
		}
	}
	$effect(() => {
		transcriptInput;
		transcriptTextarea;
		if (browser) autoGrowInput();
	});
</script>

<div class="input-area">
	{#if pendingFiles.length > 0}
		<div class="file-preview-row">
			{#each pendingFiles as file, i (file.filename + i)}
				<div class="file-preview">
					<svg
						class="file-preview-icon"
						width="18"
						height="18"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						aria-hidden="true"
					>
						<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
						<polyline points="14 2 14 8 20 8" />
					</svg>
					<div class="file-preview-info">
						<span class="file-preview-name">{file.filename}</span>
						<span class="file-preview-size">{formatFileSize(file.size)}</span>
					</div>
					<button
						class="file-preview-remove"
						onclick={() => removePendingFile(i)}
						aria-label="移除文件"
						title="移除文件"
						type="button">&times;</button
					>
				</div>
			{/each}
		</div>
	{/if}
	{#if pendingImages.length > 0}
		<div class="image-preview-row">
			{#each pendingImages as img, i (img.data + i)}
				<div class="image-preview">
					<img src={imageDataUrl(img)} alt="待发送图片" />
					<button
						class="image-preview-remove"
						onclick={() => removePendingImage(i)}
						aria-label="移除图片"
						type="button">&times;</button
					>
				</div>
			{/each}
		</div>
	{/if}
	<div class="input-row">
		<textarea
			bind:this={transcriptTextarea}
			rows="1"
			placeholder={activeSessionId
				? '追加指令，Enter 发送，Shift+Enter 换行'
				: `输入指令，Enter 发送，或按 ${hotkeyBinding} 录音`}
			bind:value={transcriptInput}
			onkeydown={handleKeydown}
			onpaste={handlePaste}
			class="md-input chat-input"
			autocomplete="off"
		></textarea>
	</div>
	<div class="toolbar-row">
		<div class="toolbar-left">
			{@render toolbarLeft?.()}
		</div>
		<div class="toolbar-right">
			<button
				class="md-icon-button file-btn"
				onclick={() => attachFileInput?.click()}
				aria-label="添加附件"
				title="添加图片或文件"
				type="button"
			>
				<svg
					width="20"
					height="20"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<path
						d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"
					/>
				</svg>
			</button>
			<input
				hidden
				type="file"
				multiple
				bind:this={attachFileInput}
				onchange={handleAttachSelect}
			/>
			<button
				class="md-icon-button record-btn"
				class:recording={recordingState.isRecording}
				onclick={handleRecordClick}
				aria-label={recordingState.isRecording ? '停止录音' : '开始录音'}
				title={recordingState.isRecording ? '停止录音' : '开始录音'}
				type="button"
			>
				{#if recordingState.isRecording}
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						><rect x="6" y="6" width="12" height="12" rx="2" /></svg
					>
				{:else}
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						><path d="M12 2a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" /><path
							d="M19 10v1a7 7 0 0 1-14 0v-1"
						/><line x1="12" y1="19" x2="12" y2="22" /></svg
					>
				{/if}
			</button>
			{@render toolbarRight?.()}
			<button
				class="md-icon-button send-btn"
				class:stop-mode={stopMode}
				onclick={stopMode ? () => onstop?.() : handleSubmit}
				disabled={!hasInput && !isGenerating && !sessionRunning}
				aria-label={hasInput ? '发送' : stopMode ? '停止会话' : '发送'}
				title={hasInput ? '发送' : stopMode ? '停止会话' : '发送'}
				type="button"
			>
				{#if hasInput}
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
					>
						<line x1="12" y1="19" x2="12" y2="5" />
						<polyline points="5 12 12 5 19 12" />
					</svg>
				{:else if stopMode}
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
						><rect x="6" y="6" width="12" height="12" rx="2" /></svg
					>
				{:else}
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
					>
						<line x1="12" y1="19" x2="12" y2="5" />
						<polyline points="5 12 12 5 19 12" />
					</svg>
				{/if}
			</button>
		</div>
	</div>
</div>

<style>
	.input-area {
		background: var(--md-sys-color-surface-container-low);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg) var(--md-sys-space-md);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}

	.image-preview-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.image-preview {
		position: relative;
		width: 64px;
		height: 64px;
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		border: 1px solid var(--md-sys-color-outline-variant);
	}
	.image-preview img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.image-preview-remove {
		position: absolute;
		top: 2px;
		right: 2px;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		border: none;
		background: rgba(0, 0, 0, 0.6);
		color: #fff;
		font-size: 13px;
		line-height: 1;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.file-preview-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.file-preview {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		max-width: 260px;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-high);
	}
	.file-preview-icon {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-preview-info {
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.file-preview-name {
		font-size: 12px;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 170px;
	}
	.file-preview-size {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-preview-remove {
		width: 20px;
		height: 20px;
		margin-left: auto;
		border-radius: 50%;
		border: none;
		flex-shrink: 0;
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		line-height: 1;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.file-preview-remove:hover {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.file-btn {
		flex-shrink: 0;
	}

	.input-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		align-items: flex-end;
	}
	.chat-input {
		--chat-pad: 8px;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-medium);
		min-height: 44px;
		height: auto;
		flex: 1;
		min-width: 0;
		padding: var(--chat-pad) var(--md-sys-space-md);
		resize: none;
		overflow-y: auto;
		line-height: 1.45;
		font-size: 14px;
	}
	.chat-input::placeholder {
		/* Placeholder line-height tracks the balanced padding so it stays
		   vertically centered exactly like the (balanced) input text. */
		line-height: calc(44px - 2 * var(--chat-pad) - 2px);
	}
	.chat-input:hover {
		border-color: var(--md-sys-color-outline-variant);
	}
	.chat-input:focus {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
		padding: var(--chat-pad) calc(var(--md-sys-space-md) - 1px);
	}

	.toolbar-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-left {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-right {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-left: auto;
	}
	.toolbar-row :global(.md-btn) {
		height: 40px;
		padding: 0 var(--md-sys-space-md);
		font-size: 13px;
	}
	.toolbar-row :global(.md-icon-button) {
		width: 40px;
		height: 40px;
		min-width: 40px;
		min-height: 40px;
		padding: 0;
	}
	.record-btn {
		flex-shrink: 0;
	}
	.record-btn.recording {
		--_ib-fg: var(--md-sys-color-error);
		--_ib-bg: var(--md-sys-color-error-container);
	}
	.send-btn {
		flex-shrink: 0;
		--_ib-fg: var(--md-sys-color-on-primary);
		--_ib-bg: var(--md-sys-color-primary);
		--_ib-state: var(--md-sys-color-on-primary);
	}
	.send-btn:hover {
		box-shadow: var(--md-sys-elevation-1);
	}
	.send-btn.stop-mode {
		--_ib-fg: var(--md-sys-color-on-error);
		--_ib-bg: var(--md-sys-color-error);
		--_ib-state: var(--md-sys-color-on-error);
	}
</style>
