<script>
	/**
	 * Composer — the single message input boundary. It delegates the existing
	 * attachment, voice, send and stop behavior to InputRouter while keeping
	 * route orchestration outside the reusable component.
	 */
	import InputRouter from './InputRouter.svelte';
	let { toolbarLeft = undefined, toolbarRight = undefined, ...restProps } = $props();
</script>

<InputRouter {...restProps} {toolbarLeft} {toolbarRight} />
