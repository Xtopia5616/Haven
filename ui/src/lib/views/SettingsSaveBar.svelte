<script lang="ts">
	import MaterialButton from '$lib/MaterialButton.svelte';

	type SaveState = 'idle' | 'saving' | 'saved' | 'error';
	interface Props {
		dirty: boolean;
		dirtySectionLabels: string[];
		saveState: SaveState;
		saveError: string;
		onDiscard: () => void;
		onSave: () => Promise<void>;
	}

	let {
		dirty,
		dirtySectionLabels,
		saveState,
		saveError,
		onDiscard,
		onSave,
	}: Props = $props();
</script>

<div class="save-bar">
	{#if saveState === 'error'}
		<p class="save-error" role="alert">{saveError}</p>
	{/if}
	{#if dirty}
		<div class="save-summary" aria-live="polite">
			<strong>有未保存更改</strong>
			<span>{dirtySectionLabels.length ? dirtySectionLabels.join('、') : '设置'}</span>
		</div>
	{/if}
	<div class="save-actions">
		{#if dirty}
			<MaterialButton
				variant="text"
				className="save-action-btn"
				label="放弃"
				onclick={onDiscard}
				disabled={saveState === 'saving'}
			/>
		{/if}
		<div class="save-button-status" aria-live="polite" aria-busy={saveState === 'saving'}>
			<MaterialButton
				variant="outlined"
				className="save-action-btn save-btn--dirty"
				label={saveState === 'saving' ? '保存中…' : dirty ? '保存更改' : '保存'}
				ariaLabel={saveState === 'saving' ? '正在保存设置' : '保存设置更改'}
				onclick={onSave}
				disabled={!dirty || saveState === 'saving'}
			/>
		</div>
	</div>
</div>

<style>
	.save-bar {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm) var(--md-comp-toolbar-gap);
		min-width: 0;
	}
	.save-error {
		flex: 1 1 100%;
		margin: 0;
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
		max-width: min(28rem, 42vw);
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
		overflow-wrap: anywhere;
	}
	:global(.save-action-btn) {
		white-space: nowrap;
	}
	:global(.save-btn--dirty.md-btn--outlined) {
		--_btn-bg: var(--md-sys-color-surface-container-high);
		--_btn-fg: var(--md-sys-color-on-surface);
		--_btn-border: 1px solid var(--md-sys-color-outline-variant);
		--_btn-state: var(--md-sys-color-on-surface);
	}
	:global(.save-btn--dirty.md-btn--outlined:enabled:hover) {
		--_btn-bg: var(--md-sys-color-surface-container-highest);
	}
	.save-button-status {
		display: flex;
		min-width: 0;
	}
	.save-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 0 0 auto;
	}
	@media screen and (max-width: 520px) {
		.save-summary {
			flex: 1 1 100%;
			max-width: none;
		}
		.save-actions {
			margin-left: auto;
		}
	}
</style>
