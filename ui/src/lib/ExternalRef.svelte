<script lang="ts">
	import { EXT_REF_CLASS, EXT_REF_TITLE, handleExtRefEvent } from '$lib/externalRef.ts';

	interface Props {
		target: string;
		class?: string;
	}

	let { target, class: className = '' }: Props = $props();

	function onKey(e: KeyboardEvent) {
		if (e.key !== 'Enter' && e.key !== ' ') return;
		e.preventDefault();
		// Re-use the mouse handler shape: synthesize a click-like event target.
		handleExtRefEvent({
			type: 'click',
			ctrlKey: e.ctrlKey,
			metaKey: e.metaKey,
			target: e.currentTarget,
			preventDefault() {},
			stopPropagation() {},
		});
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
