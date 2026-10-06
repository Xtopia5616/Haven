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
	}

	let { visible, dirty, dirtySectionLabels, saveState, saveError, onDiscard, onSave }: Props =
		$props();
</script>

<div
	class="save-bar save-bar--bottom-edge md-toolbar"
	class:save-bar--hidden={!visible}
	aria-hidden={!visible}
	inert={!visible}
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
			className="save-action-btn"
			label="放弃"
			onclick={onDiscard}
			disabled={saveState === 'saving'}
		/>
		<div class="save-button-status" aria-live="polite" aria-busy={saveState === 'saving'}>
			<MaterialButton
				variant="filled"
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
	}
	.save-bar--hidden,
	.save-bar__dirty-content--hidden {
		visibility: hidden;
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
		width: 96px;
		min-width: 96px;
	}
	:global(.save-btn--dirty) {
		box-shadow: var(--md-sys-elevation-2);
	}
	.save-button-status {
		display: inline-flex;
	}
	.save-actions {
		display: flex;
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
			align-items: stretch;
			flex-direction: column;
		}
		.save-actions,
		.save-actions :global(.md-btn),
		.save-button-status {
			width: 100%;
		}
		.save-actions > :global(.md-btn),
		.save-actions > .save-button-status {
			flex: 1 1 0;
			min-width: 0;
		}
		.save-actions :global(.md-btn) {
			min-width: 0;
		}
		:global(.save-action-btn) {
			width: 100%;
		}
		.save-actions {
			align-items: stretch;
		}
	}
</style>
