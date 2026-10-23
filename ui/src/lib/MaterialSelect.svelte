<script lang="ts">
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import Icon from './Icon.svelte';
	import type { IconName } from './icons.ts';
	import type { ControlWidth } from './controlWidth.ts';
	import type { SelectOption } from './selectOption.ts';

	type MenuRow =
		| { kind: 'group'; label: string }
		| {
				kind: 'option';
				label: string;
				description?: string;
				value: string;
				id: string;
				index: number;
		  };

	interface Props {
		value?: string;
		options?: SelectOption[];
		onChange?: (value: string) => void;
		id?: string;
		ariaLabel?: string;
		title?: string;
		iconOnly?: boolean;
		icon?: IconName;
		appearance?: 'standard' | 'toolbar';
		width?: ControlWidth;
		disabled?: boolean;
	}

	let {
		value = '',
		options = [],
		onChange,
		id = undefined,
		ariaLabel = '',
		title = '',
		iconOnly = false,
		icon = 'folder',
		appearance = 'standard',
		width = 'fill',
		disabled = false,
	}: Props = $props();
	const generatedId = $props.id();
	let controlId = $derived(id ?? generatedId);
	let listboxId = $derived(`${controlId}-listbox`);

	let open = $state(false);
	let selectedLabel = $derived(options.find((o) => o.value === value)?.label || value);
	let dropdownRef: HTMLDivElement | null = null;
	let activeOptionIndex = $state(0);

	/** Flatten options into rows with optional group headers. */
	let menuRows = $derived.by(() => {
		const rows: MenuRow[] = [];
		let lastGroup: string | null = null;
		let optionIndex = 0;
		for (const opt of options) {
			const group = opt.group || null;
			if (group && group !== lastGroup) {
				rows.push({ kind: 'group', label: group });
				lastGroup = group;
			}
			rows.push({
				kind: 'option',
				label: opt.label,
				description: opt.description,
				value: opt.value,
				id: `${listboxId}-option-${optionIndex}`,
				index: optionIndex,
			});
			optionIndex += 1;
		}
		return rows;
	});
	let optionRows = $derived(
		menuRows.filter((row): row is Extract<MenuRow, { kind: 'option' }> => row.kind === 'option'),
	);
	let activeOptionId = $derived(optionRows[activeOptionIndex]?.id);

	function selectedOptionIndex(): number {
		return optionRows.findIndex((row) => row.value === value);
	}

	function openMenu(index = selectedOptionIndex()): void {
		if (disabled || optionRows.length === 0) return;
		activeOptionIndex = index >= 0 ? index : 0;
		open = true;
	}

	function toggle(): void {
		if (disabled) return;
		if (open) open = false;
		else openMenu();
	}

	function select(selectedValue: string): void {
		if (disabled) return;
		open = false;
		onChange?.(selectedValue);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (disabled) return;
		if (optionRows.length === 0) {
			open = false;
			return;
		}
		if (event.key === 'Escape') {
			if (open) {
				event.preventDefault();
				open = false;
			}
			return;
		}

		if (!open) {
			if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
				event.preventDefault();
				const selected = selectedOptionIndex();
				openMenu(
					selected >= 0
						? selected
						: event.key === 'ArrowDown'
							? 0
							: optionRows.length - 1,
				);
			} else if (event.key === 'Enter' || event.key === ' ') {
				event.preventDefault();
				openMenu();
			}
			return;
		}

		if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			event.preventDefault();
			const offset = event.key === 'ArrowDown' ? 1 : -1;
			activeOptionIndex =
				(activeOptionIndex + offset + optionRows.length) % optionRows.length;
		} else if (event.key === 'Home' || event.key === 'End') {
			event.preventDefault();
			activeOptionIndex = event.key === 'Home' ? 0 : optionRows.length - 1;
		} else if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			const activeOption = optionRows[activeOptionIndex];
			if (activeOption) select(activeOption.value);
		}
	}

	function handleBlur(event: FocusEvent): void {
		// The option is inside the same control. Defer the close until focus has
		// settled so a pointer click cannot lose its target between blur and click.
		if (event.relatedTarget instanceof Node && dropdownRef?.contains(event.relatedTarget))
			return;
		setTimeout(() => {
			if (!dropdownRef?.contains(document.activeElement)) open = false;
		}, 0);
	}

	function handleWindowPointerdown(event: PointerEvent): void {
		if (open && !(event.target instanceof Node && dropdownRef?.contains(event.target))) {
			open = false;
		}
	}
</script>

<svelte:window onpointerdown={handleWindowPointerdown} />

<div
	class="md-select-container"
	data-width={width}
	data-appearance={appearance}
	data-icon-only={iconOnly ? 'true' : undefined}
	bind:this={dropdownRef}
>
	<button
		id={controlId}
		class="md-select-trigger"
		class:open
		class:disabled
		data-icon-only={iconOnly ? 'true' : undefined}
		disabled={disabled}
		aria-label={ariaLabel || undefined}
		role="combobox"
		aria-controls={open ? listboxId : undefined}
		aria-activedescendant={open ? activeOptionId : undefined}
		onclick={toggle}
		onkeydown={handleKeydown}
		onblur={handleBlur}
		type="button"
		aria-haspopup="listbox"
		aria-expanded={open}
		{title}
	>
		{#if iconOnly}
			<Icon name={icon} size={16} className="md-select-icon" />
		{:else}
			<span class="md-select-value">{selectedLabel}</span>
		{/if}
		<Icon name="chevronDown" size={20} className="md-select-arrow" />
	</button>

	{#if open}
		<div
			id={listboxId}
			class="md-select-menu"
			role="listbox"
			aria-label={ariaLabel || undefined}
			in:fly={{ y: -4, duration: 300, easing: cubicOut }}
		>
			{#each menuRows as row}
				{#if row.kind === 'group'}
					<div class="md-select-group" role="presentation">{row.label}</div>
				{:else}
					<button
						id={row.id}
						tabindex="-1"
						class="md-select-option"
						class:selected={row.value === value}
						class:active={row.index === activeOptionIndex}
						role="option"
						aria-selected={row.value === value}
						onclick={() => select(row.value)}
						onpointermove={() => (activeOptionIndex = row.index)}
						onmousedown={(e) => e.preventDefault()}
						type="button"
					>
						<span class="md-select-option-copy">
							<span class="md-select-option-label">{row.label}</span>
							{#if row.description}
								<span class="md-select-option-description">{row.description}</span>
							{/if}
						</span>
						{#if appearance === 'toolbar' && row.value === value}
							<Icon name="check" size={16} strokeWidth={2.5} className="md-select-option-check" />
						{/if}
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
	.md-select-trigger[data-icon-only='true'] {
		justify-content: center;
		gap: var(--md-sys-space-xs);
		width: var(--md-comp-icon-button-size);
		height: var(--md-comp-icon-button-size);
		padding: 0;
		border-radius: var(--md-comp-button-radius);
	}
	.md-select-trigger[data-icon-only='true'].open,
	.md-select-trigger[data-icon-only='true']:focus-visible {
		padding: 0;
	}
	.md-select-trigger[data-icon-only='true'] :global(.md-select-arrow) {
		width: 12px;
		height: 12px;
		margin-left: 0;
	}
	.md-select-trigger:disabled {
		color: var(--md-sys-color-on-surface-variant);
		border-color: var(--md-sys-color-outline-variant);
		cursor: not-allowed;
	}
	.md-select-trigger:disabled :global(.md-select-arrow) {
		opacity: 0.5;
	}
	.md-select-trigger.open,
	.md-select-trigger:focus-visible {
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
	.md-select-option-copy {
		display: flex;
		flex: 1 1 auto;
		flex-direction: column;
		min-width: 0;
		gap: 2px;
	}
	.md-select-option-description {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: normal;
	}
	:global(.md-select-option-check) {
		flex: 0 0 auto;
		margin-left: var(--md-sys-space-md);
		color: var(--md-sys-color-primary);
	}
	:global(.md-select-arrow) {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		transition: transform var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
		margin-left: var(--md-sys-space-sm);
	}
	.md-select-trigger.open :global(.md-select-arrow) {
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
	.md-select-container[data-icon-only='true'] .md-select-menu {
		right: auto;
		width: min(
			var(--md-comp-settings-control-width),
			calc(100vw - 2 * var(--md-sys-space-md))
		);
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
		min-width: 0;
		transition: background-color var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
		position: relative;
	}
	.md-select-option-label {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.md-select-option:hover {
		background: var(--md-sys-color-surface-container-high);
	}
	.md-select-option.active {
		background: var(--md-sys-color-surface-container-high);
	}
	.md-select-option:active {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-select-option.selected {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		outline: var(--md-sys-selection-outline-width) solid
			var(--md-sys-selection-outline-color);
		outline-offset: var(--md-sys-selection-outline-offset);
	}
	.md-select-container[data-appearance='toolbar'] {
		width: fit-content;
		max-width: 100%;
		flex: 0 1 auto;
	}
	.md-select-container[data-appearance='toolbar'] .md-select-trigger {
		width: fit-content;
		max-width: 100%;
		min-width: 0;
		height: var(--md-comp-toolbar-height);
		gap: var(--md-sys-space-xs);
		padding-inline: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-label-large-size);
		line-height: var(--md-sys-typescale-label-large-line-height);
		color: var(--md-sys-color-on-surface-variant);
		background: transparent;
		border-color: transparent;
		border-radius: var(--md-sys-shape-full);
	}
	.md-select-container[data-appearance='toolbar'] .md-select-trigger:hover,
	.md-select-container[data-appearance='toolbar'] .md-select-trigger.open {
		color: var(--md-sys-color-on-surface);
		background: var(--md-sys-color-surface-container-high);
		border-color: transparent;
	}
	.md-select-container[data-appearance='toolbar'] .md-select-trigger.open,
	.md-select-container[data-appearance='toolbar'] .md-select-trigger:focus-visible {
		padding-inline: var(--md-sys-space-sm);
		border-width: 1px;
		outline: none;
		box-shadow: var(--md-sys-focus-ring);
	}
	.md-select-container[data-appearance='toolbar'] :global(.md-select-arrow) {
		width: 16px;
		height: 16px;
		margin-left: 0;
	}
	.md-select-container[data-appearance='toolbar'] .md-select-menu {
		left: auto;
		right: 0;
		width: max-content;
		min-width: 228px;
		max-width: min(320px, calc(100vw - 32px));
		padding: var(--md-sys-space-2xs);
		border-radius: var(--md-sys-shape-large);
	}
	.md-select-container[data-appearance='toolbar'] .md-select-option {
		min-height: 50px;
		height: auto;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-medium);
	}
	.md-select-container[data-appearance='toolbar'] .md-select-option.selected {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
		font-weight: 600;
		outline: none;
	}
</style>
