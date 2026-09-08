<script>
	import RecordingIndicator from '$lib/RecordingIndicator.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';

	let mode = $state('recording');
	let duration = $state(7);
	$effect(() => {
		const t = setInterval(() => {
			duration += 1;
		}, 1000);
		return () => clearInterval(t);
	});
</script>

<svelte:head><title>Recording Preview</title></svelte:head>

<div style="position: relative; min-height: 300px;">
	<MaterialButton
		variant="tonal"
		label="录音中(静默)"
		ariaPressed={mode === 'recording'}
		onclick={() => (mode = 'recording')}
	/>
	<MaterialButton
		variant="tonal"
		label="正在聆听"
		ariaPressed={mode === 'speaking'}
		onclick={() => (mode = 'speaking')}
	/>
	<MaterialButton
		variant="tonal"
		label="转写中"
		ariaPressed={mode === 'processing'}
		onclick={() => (mode = 'processing')}
	/>

	{#if mode === 'recording'}
		<RecordingIndicator isRecording={true} vadState="silent" duration={duration} onCancel={async () => {}} />
	{:else if mode === 'speaking'}
		<RecordingIndicator isRecording={true} vadState="speech" duration={duration} onCancel={async () => {}} />
	{:else}
		<RecordingIndicator processing={true} duration={duration} onCancel={async () => {}} />
	{/if}
</div>
