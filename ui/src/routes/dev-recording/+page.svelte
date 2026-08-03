<script>
	import RecordingIndicator from '$lib/RecordingIndicator.svelte';

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
	<button onclick={() => (mode = 'recording')}>录音中(静默)</button>
	<button onclick={() => (mode = 'speaking')}>正在聆听</button>
	<button onclick={() => (mode = 'processing')}>转写中</button>

	{#if mode === 'recording'}
		<RecordingIndicator isRecording={true} vadState="silent" duration={duration} onCancel={async () => {}} />
	{:else if mode === 'speaking'}
		<RecordingIndicator isRecording={true} vadState="speech" duration={duration} onCancel={async () => {}} />
	{:else}
		<RecordingIndicator processing={true} duration={duration} onCancel={async () => {}} />
	{/if}
</div>
