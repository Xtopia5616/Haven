<script>
	import { EXT_REF_CLASS, EXT_REF_TITLE, handleExtRefEvent } from '$lib/externalRef.ts';

	/** @type {{ target: string, class?: string }} */
	let { target, class: className = '' } = $props();

	/** @param {KeyboardEvent} e */
	function onKey(e) {
		if (e.key !== 'Enter' && e.key !== ' ') return;
		e.preventDefault();
		// Re-use the mouse handler shape: synthesize a click-like event target.
		handleExtRefEvent(
			/** @type {any} */ ({
				type: 'click',
				ctrlKey: e.ctrlKey,
				metaKey: e.metaKey,
				target: e.currentTarget,
				preventDefault() {},
				stopPropagation() {},
			}),
		);
	}
</script>

<span
	class="{EXT_REF_CLASS} {className}"
	role="link"
	tabindex="0"
	data-target={target}
	title={EXT_REF_TITLE}
	onclick={handleExtRefEvent}
	oncontextmenu={handleExtRefEvent}
	onkeydown={onKey}>{target}</span
>
