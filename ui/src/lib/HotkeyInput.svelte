<script>
	// A key-binding capture field. Instead of typing the combo as text, the
	// user clicks the field and presses the desired key combination; the
	// formatted string (matching the backend `parse_shortcut` format, e.g.
	// "Ctrl+Shift+Space") is emitted via `onChange`.
	//
	// Supported keys mirror `parse_shortcut` (crates/app-binary/src/lib.rs):
	// modifiers Ctrl/Shift/Alt/Super + a-z, Space, Enter, Tab, F1-F12. The
	// shared formatting lives in `hotkeyFormat.ts` so it can be unit-tested
	// for parity with the backend; see `hotkeyFormat.test.ts`.
	import { formatCombo } from './hotkeyFormat.ts';
	let { value = '', onChange, id = undefined, placeholder = '点击并按下快捷键' } = $props();

	let listening = $state(false);

	function startListening() {
		listening = true;
	}

	function stopListening() {
		listening = false;
	}

	function handleKeydown(e) {
		if (!listening) return;
		// Prevent the browser from acting on the combo while capturing.
		e.preventDefault();
		e.stopPropagation();
		// Escape cancels capture without changing the value.
		if (e.key === 'Escape') {
			stopListening();
			return;
		}
		const combo = formatCombo(e);
		if (combo) {
			onChange?.(combo);
			stopListening();
		}
	}

	function handleBlur() {
		stopListening();
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="hotkey-input-wrap">
	<button
		{id}
		type="button"
		class="md-input hotkey-input"
		class:listening
		onclick={startListening}
		onblur={handleBlur}
		aria-label="快捷键绑定"
	>
		{#if listening}
			<span class="hotkey-listening-hint">按下快捷键组合…</span>
		{:else}
			<span class="hotkey-value">{value || placeholder}</span>
		{/if}
	</button>
</div>

<style>
	.hotkey-input-wrap {
		width: 100%;
	}
	.hotkey-input {
		display: flex;
		align-items: center;
		cursor: pointer;
		text-align: left;
		font-family: var(--md-sys-typescale-mono, monospace);
		font-size: 14px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.hotkey-input.listening {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
	}
	.hotkey-listening-hint {
		color: var(--md-sys-color-primary);
		font-style: italic;
	}
	.hotkey-value {
		color: var(--md-sys-color-on-surface);
	}
</style>
