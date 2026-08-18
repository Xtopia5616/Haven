<script>
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	let {
		value = '',
		options = [],
		placeholder = '',
		id = undefined,
		loading = false,
		onChange = undefined,
		onFocus = undefined,
	} = $props();

	let text = $state('');
	let open = $state(false);
	let menuId = $state('ma-menu-' + Math.random().toString(36).slice(2, 8));
	/** @type {HTMLDivElement | null} */
	let rootRef = null;
	/** @type {HTMLInputElement | null} */
	let inputRef = null;

	// Sync when the parent changes the value from outside (settings load,
	// external selection). Parent-driven updates converge with typing since
	// onChange already mirrors every keystroke.
	$effect(() => {
		text = value;
	});

	let filtered = $derived(
		(text
			? options.filter((o) =>
					(o.label || o.value).toLowerCase().includes(text.toLowerCase())
				)
			: options
		).slice(0, 100)
	);

	function onInput() {
		open = true;
		onChange?.(text);
	}

	/**
	 * @param {any} opt
	 */
	function pick(opt) {
		text = opt.value;
		onChange?.(opt.value);
		open = false;
		inputRef?.focus();
	}

	/**
	 * @param {KeyboardEvent} e
	 */
	function handleKeydown(e) {
		if (e.key === 'Escape') {
			open = false;
		}
	}

	function handleBlur() {
		// Let option mousedowns register before closing.
		setTimeout(() => {
			if (!rootRef?.contains(document.activeElement)) open = false;
		}, 150);
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="ma-root" bind:this={rootRef}>
	<input
		{id}
		type="text"
		class="md-input"
		class:ma-open={open}
		bind:this={inputRef}
		bind:value={text}
		{placeholder}
		autocomplete="off"
		oninput={onInput}
		onfocus={() => {
			open = true;
			onFocus?.();
		}}
		onblur={handleBlur}
		role="combobox"
		aria-expanded={open}
		aria-autocomplete="list"
		aria-controls={menuId}
	/>

	{#if open && (options.length > 0 || loading)}
		<div class="ma-menu" id={menuId} role="listbox" in:fly={{ y: -4, duration: 300, easing: cubicOut }}>
			{#if filtered.length > 0}
				{#each filtered as opt}
					<button
						class="ma-option"
						class:selected={opt.value === text}
						role="option"
						aria-selected={opt.value === text}
						onclick={() => pick(opt)}
						onmousedown={(e) => e.preventDefault()}
						type="button"
					>
						<span class="ma-option-label">{opt.label}</span>
						<span class="ma-option-value">{opt.value}</span>
					</button>
				{/each}
			{:else if loading}
				<div class="ma-empty">Fetching models…</div>
			{:else}
				<div class="ma-empty">No matching models</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.ma-root {
		position: relative;
		width: 100%;
		flex: 1;
		min-width: 0;
	}
	.ma-root input {
		width: 100%;
	}
	input.ma-open {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
		padding: 0 calc(var(--md-sys-space-lg) - 1px);
	}
	.ma-menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		right: 0;
		max-height: 240px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		box-shadow: var(--md-sys-elevation-3);
		z-index: 100;
	}
	.ma-option {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		width: 100%;
		padding: 0 var(--md-sys-space-lg);
		height: 40px;
		font-family: inherit;
		font-size: 14px;
		color: var(--md-sys-color-on-surface);
		background: transparent;
		border: none;
		cursor: pointer;
		text-align: left;
		transition: background-color var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.ma-option:hover {
		background: var(--md-sys-color-surface-container-high);
	}
	.ma-option:active {
		background: var(--md-sys-color-surface-container-highest);
	}
	.ma-option.selected {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.ma-option-label {
		flex-shrink: 0;
	}
	.ma-option-value {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 12px;
		opacity: 0.7;
	}
	.ma-empty {
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		font-size: 13px;
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
