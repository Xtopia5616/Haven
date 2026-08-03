<script>
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	let { value = '', options = [], onChange, id = undefined } = $props();

	let open = $state(false);
	let selectedLabel = $derived(options.find((o) => o.value === value)?.label || value);
	/** @type {HTMLDivElement | null} */
	let dropdownRef = null;

	function toggle() {
		open = !open;
	}

	function select(val) {
		onChange?.(val);
		open = false;
	}

	function handleKeydown(e) {
		if (e.key === 'Escape') open = false;
	}

	function handleBlur() {
		open = false;
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="md-select-container" bind:this={dropdownRef}>
	<button
		{id}
		class="md-select-trigger"
		class:open
		onclick={toggle}
		onblur={handleBlur}
		type="button"
		aria-haspopup="listbox"
		aria-expanded={open}
	>
		<span class="md-select-value">{selectedLabel}</span>
		<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="md-select-arrow">
			<path d="M6 9l6 6 6-6"/>
		</svg>
	</button>

	{#if open}
		<div class="md-select-menu" role="listbox" in:fly={{ y: -4, duration: 300, easing: cubicOut }}>
			{#each options as opt}
				<button
					class="md-select-option"
					class:selected={opt.value === value}
					role="option"
					aria-selected={opt.value === value}
					onclick={() => select(opt.value)}
					onmousedown={(e) => e.preventDefault()}
					type="button"
				>
					{opt.label}
				</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	.md-select-container {
		position: relative;
		width: 100%;
	}
	.md-select-trigger {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		height: var(--md-comp-textfield-container-height);
		padding: 0 var(--md-sys-space-lg);
		font-family: inherit;
		font-size: 15px;
		color: var(--md-sys-color-on-surface);
		background: transparent;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-comp-textfield-corner);
		cursor: pointer;
		text-align: left;
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-width var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-emphasized);
		position: relative;
	}
	.md-select-trigger:hover {
		border-color: var(--md-sys-color-on-surface);
	}
	.md-select-trigger.open,
	.md-select-trigger:focus {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
		padding: 0 calc(var(--md-sys-space-lg) - 1px);
	}
	.md-select-value {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.md-select-arrow {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: transform var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		margin-left: var(--md-sys-space-sm);
	}
	.md-select-trigger.open .md-select-arrow {
		transform: rotate(180deg);
	}
	.md-select-menu {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		right: 0;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		box-shadow: var(--md-sys-elevation-3);
		z-index: 100;
		overflow: hidden;
	}
	.md-select-option {
		display: flex;
		align-items: center;
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
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
		position: relative;
	}
	.md-select-option:hover {
		background: var(--md-sys-color-surface-container-high);
	}
	.md-select-option:active {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-select-option.selected {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
</style>