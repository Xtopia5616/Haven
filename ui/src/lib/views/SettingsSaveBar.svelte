<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';

	type SaveState = 'idle' | 'saving' | 'saved' | 'error';
	interface Props {
		visible: boolean;
		dirty: boolean;
		dirtySectionLabels: string[];
		saveState: SaveState;
		saveError: string;
		onDiscard: () => void;
		onSave: () => Promise<void>;
		onHeightChange: (height: number) => void;
	}

	let {
		visible,
		dirty,
		dirtySectionLabels,
		saveState,
		saveError,
		onDiscard,
		onSave,
		onHeightChange,
	}: Props = $props();
	let saveBarElement = $state<HTMLDivElement | null>(null);
	let reportedHeight = 0;

	function reportHeight() {
		const height = Math.ceil(saveBarElement?.getBoundingClientRect().height ?? 0);
		if (height !== reportedHeight) {
			reportedHeight = height;
			onHeightChange(height);
		}
	}

	$effect(() => {
		const element = saveBarElement;
		if (!element) return;
		const observer =
			typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(reportHeight);
		observer?.observe(element);
		reportHeight();
		return () => observer?.disconnect();
	});
</script>

<div
	class="save-bar save-bar--bottom-edge md-toolbar"
	class:save-bar--hidden={!visible}
	aria-hidden={!visible}
	inert={!visible}
	bind:this={saveBarElement}
>
	{#if saveState === 'error'}
		<p class="save-error" role="alert">{saveError}</p>
	{/if}
	<div class="save-summary" class:save-bar__dirty-content--hidden={!dirty} aria-live="polite">
		<strong>有未保存更改</strong>
		<span>{dirtySectionLabels.length ? dirtySectionLabels.join('、') : '设置'}</span>
	</div>
	<div class="save-actions" class:save-bar__dirty-content--hidden={!dirty} inert={!dirty}>
		<MaterialButton
			variant="outlined"
			width="fill"
			className="save-action-btn"
			label="放弃"
			onclick={onDiscard}
			disabled={saveState === 'saving'}
		/>
		<div class="save-button-status" aria-live="polite" aria-busy={saveState === 'saving'}>
			<MaterialButton
				variant="filled"
				width="fill"
				className="save-action-btn save-btn--dirty"
				label={saveState === 'saving' ? '保存中…' : '保存'}
				onclick={onSave}
				disabled={saveState === 'saving'}
			/>
		</div>
	</div>
</div>

<style>
	.save-bar {
		position: relative;
		width: 100%;
		max-width: none;
		pointer-events: auto;
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: var(--md-comp-toolbar-gap);
		padding: var(--md-sys-space-lg);
		border-top: 1px solid
			color-mix(in srgb, var(--md-sys-color-outline-variant) 72%, transparent);
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--md-sys-color-surface-container-lowest) 68%, transparent),
			color-mix(in srgb, var(--md-sys-color-surface-container-lowest) 94%, transparent)
		);
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
		box-shadow: 0 -8px 20px color-mix(in srgb, var(--md-sys-color-shadow) 8%, transparent);
		transition:
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			visibility 0s linear;
	}
	.save-bar--hidden,
	.save-bar__dirty-content--hidden {
		visibility: hidden;
	}
	.save-bar--hidden {
		opacity: 0;
		transform: translateY(var(--md-sys-space-sm));
		pointer-events: none;
		transition-delay: 0s, 0s, var(--md-sys-motion-duration-fast);
	}
	.save-error {
		margin: 0 auto 0 0;
		color: var(--md-sys-color-error);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.save-summary {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-2xs);
		margin-right: auto;
		min-width: 0;
	}
	.save-summary strong {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.save-summary span {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	:global(.save-action-btn) {
		width: 100%;
	}
	:global(.save-btn--dirty) {
		box-shadow: var(--md-sys-elevation-2);
	}
	.save-button-status {
		display: flex;
		min-width: 0;
	}
	.save-actions {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		width: max-content;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
	}
	@media screen and (min-width: 840px) {
		.save-bar {
			padding-inline: var(--md-sys-space-2xl);
		}
	}
	@media screen and (min-width: 840px) {
		.save-bar {
			align-items: center;
			flex-direction: row;
		}
	}
</style>
