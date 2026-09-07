<script>
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	let { value = '', options = [], onChange, id = undefined, ariaLabel = '' } = $props();

	let open = $state(false);
	let selectedLabel = $derived(options.find((o) => o.value === value)?.label || value);
	/** @type {HTMLDivElement | null} */
	let dropdownRef = null;

	/** Flatten options into rows with optional group headers. */
	let menuRows = $derived.by(() => {
		/** @type {{ kind: 'group' | 'option', label: string, value?: string }[]} */
		const rows = [];
		let lastGroup = null;
		for (const opt of options) {
			const group = opt.group || null;
			if (group && group !== lastGroup) {
				rows.push({ kind: 'group', label: group });
				lastGroup = group;
			}
			rows.push({ kind: 'option', label: opt.label, value: opt.value });
		}
		return rows;
	});

	function toggle() {
		open = !open;
	}

	/**
	 * @param {any} val
	 */
	function select(val) {
		open = false;
		onChange?.(val);
	}

	/**
	 * @param {KeyboardEvent} e
	 */
	function handleKeydown(e) {
		if (e.key === 'Escape') open = false;
	}

	/** @param {FocusEvent} e */
	function handleBlur(e) {
		// The option is inside the same control. Defer the close until focus has
		// settled so a pointer click cannot lose its target between blur and click.
		if (e.relatedTarget && dropdownRef?.contains(/** @type {Node} */ (e.relatedTarget))) return;
		setTimeout(() => {
			if (!dropdownRef?.contains(document.activeElement)) open = false;
		}, 0);
	}

	/** @param {PointerEvent} e */
	function handleWindowPointerdown(e) {
		if (open && !dropdownRef?.contains(/** @type {Node} */ (e.target))) open = false;
	}
</script>

<svelte:window onkeydown={handleKeydown} onpointerdown={handleWindowPointerdown} />

<div class="md-select-container" bind:this={dropdownRef}>
	<button
		{id}
		class="md-select-trigger"
		class:open
		aria-label={ariaLabel || undefined}
		onclick={toggle}
		onblur={handleBlur}
		type="button"
		aria-haspopup="listbox"
		aria-expanded={open}
	>
		<span class="md-select-value">{selectedLabel}</span>
		<svg
			width="20"
			height="20"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
			class="md-select-arrow"
		>
			<path d="M6 9l6 6 6-6" />
		</svg>
	</button>

	{#if open}
		<div
			class="md-select-menu"
			role="listbox"
			in:fly={{ y: -4, duration: 300, easing: cubicOut }}
		>
			{#each menuRows as row}
				{#if row.kind === 'group'}
					<div class="md-select-group" role="presentation">{row.label}</div>
				{:else}
					<button
						class="md-select-option"
						class:selected={row.value === value}
						role="option"
						aria-selected={row.value === value}
						onclick={() => select(row.value)}
						onmousedown={(e) => e.preventDefault()}
						type="button"
					>
						{row.label}
					</button>
				{/if}
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
		font-size: var(--md-sys-typescale-body-large-size);
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-on-surface);
		background: transparent;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-comp-textfield-corner);
		cursor: pointer;
		text-align: left;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
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
		transition: transform var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
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
		max-height: min(360px, 60vh);
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		box-shadow: var(--md-sys-elevation-3);
		z-index: 100;
	}
	.md-select-group {
		padding: 10px var(--md-sys-space-lg) 4px;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		letter-spacing: 0.06em;
		line-height: var(--md-sys-typescale-label-small-line-height);
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		user-select: none;
	}
	.md-select-option {
		display: flex;
		align-items: center;
		width: 100%;
		padding: 0 var(--md-sys-space-lg);
		height: 40px;
		font-family: inherit;
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-on-surface);
		background: transparent;
		border: none;
		cursor: pointer;
		text-align: left;
		transition: background-color var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
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
