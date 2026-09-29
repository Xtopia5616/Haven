<script>
	import MaterialDialog from './MaterialDialog.svelte';

	let visible = $state(true);
</script>

<button onclick={() => (visible = false)}>Hide parent</button>

{#if visible}
	<MaterialDialog open={true} title="Transition test" onClose={() => {}}>
		{#snippet children()}{/snippet}
	</MaterialDialog>
{/if}
