<script>
	/**
	 * Composer — the single message input boundary. It delegates the existing
	 * attachment, voice, send and stop behavior to InputRouter while keeping
	 * route orchestration outside the reusable component.
	 */
	import InputRouter from './InputRouter.svelte';
	let { toolbarLeft = undefined, toolbarRight = undefined, ...restProps } = $props();
	/** @type {any} */
	let inputRouterRef = $state(null);

	/**
	 * Preserve the imperative input API through this presentational wrapper.
	 * The chat route binds to Composer, not InputRouter, when it needs to put a
	 * rolled-back user message back into the draft.
	 *
	 * @param {string} text
	 */
	export function setDraft(text) {
		inputRouterRef?.setDraft(text);
	}
</script>

<InputRouter bind:this={inputRouterRef} {...restProps} {toolbarLeft} {toolbarRight} />
