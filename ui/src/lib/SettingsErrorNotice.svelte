<script lang="ts">
	import Icon from '$lib/Icon.svelte';
	import type { SettingsFailurePresentation } from '$lib/settingsSaveFailure.ts';

	interface Props {
		failure: SettingsFailurePresentation;
	}

	let { failure }: Props = $props();
</script>

<section class="settings-error" data-tone={failure.tone} role="alert">
	<Icon
		name={failure.tone === 'warning' ? 'alertTriangle' : 'alertCircle'}
		size={20}
		className="settings-error__icon"
	/>
	<div class="settings-error__content">
		<strong class="settings-error__title">{failure.title}</strong>
		<p class="settings-error__message">{failure.message}</p>
		<details class="settings-error__details">
			<summary>查看技术诊断</summary>
			<pre>{failure.detail}</pre>
		</details>
	</div>
</section>

<style>
	.settings-error {
		--settings-error-color: var(--md-sys-color-error);
		--settings-error-background: var(--md-sys-color-error-container);
		--settings-error-foreground: var(--md-sys-color-on-error-container);
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		min-width: 0;
		padding: var(--md-sys-space-md);
		border: 1px solid color-mix(in srgb, var(--settings-error-color) 42%, transparent);
		border-radius: var(--md-sys-shape-medium);
		background: var(--settings-error-background);
		color: var(--settings-error-foreground);
	}
	.settings-error[data-tone='warning'] {
		--settings-error-color: var(--md-sys-color-warning);
		--settings-error-background: var(--md-sys-color-warning-container);
		--settings-error-foreground: var(--md-sys-color-on-warning-container);
	}
	:global(.settings-error__icon) {
		flex: 0 0 auto;
		margin-top: 1px;
		color: var(--settings-error-color);
	}
	.settings-error__content {
		flex: 1 1 auto;
		min-width: 0;
	}
	.settings-error__title {
		display: block;
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.settings-error__message {
		margin: var(--md-sys-space-2xs) 0 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		white-space: normal;
		overflow-wrap: anywhere;
	}
	.settings-error__details {
		margin-top: var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.settings-error__details summary {
		width: fit-content;
		cursor: pointer;
		color: var(--settings-error-foreground);
		text-decoration: underline;
		text-underline-offset: 2px;
	}
	.settings-error__details pre {
		max-height: 10rem;
		margin: var(--md-sys-space-xs) 0 0;
		padding: var(--md-sys-space-sm);
		overflow: auto;
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--settings-error-foreground) 8%, transparent);
		font: inherit;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		user-select: text;
	}
</style>
