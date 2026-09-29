<script lang="ts">
	/**
	 * Composer — the single message input boundary. It delegates the existing
	 * attachment, voice, send and stop behavior to InputRouter while keeping
	 * route orchestration outside the reusable component.
	 */
	import InputRouter from './InputRouter.svelte';
	import type { ComponentProps, Snippet } from 'svelte';

	type InputRouterProps = ComponentProps<typeof InputRouter>;
	interface Props extends Omit<InputRouterProps, 'toolbarLeft' | 'toolbarRight'> {
		toolbarLeft?: Snippet;
		toolbarRight?: Snippet;
	}

	let { toolbarLeft, toolbarRight, ...restProps }: Props = $props();
	let inputRouterRef = $state<{ setDraft: (text: string) => void } | null>(null);

	/**
	 * Preserve the imperative input API through this presentational wrapper.
	 * The chat route binds to Composer, not InputRouter, when it needs to put a
	 * rolled-back user message back into the draft.
	 *
	 */
	export function setDraft(text: string) {
		inputRouterRef?.setDraft(text);
	}
</script>

<InputRouter bind:this={inputRouterRef} {...restProps} {toolbarLeft} {toolbarRight} />
