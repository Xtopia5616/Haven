<script lang="ts">
	import MaterialBadge from './MaterialBadge.svelte';
	import type { StatusTone } from './statusColors.ts';

	type StatusBadgeTone = Extract<StatusTone, 'neutral' | 'info' | 'success' | 'warning' | 'error'>;

	interface Props {
		label?: string;
		tone?: StatusBadgeTone;
		className?: string;
	}

	/**
	 * Status Badge — shared compact semantic status indicator.
	 */
	let { label = '', tone = 'neutral', className = '' }: Props = $props();
	let variant = $derived(mapToneToVariant(tone));

	function mapToneToVariant(
		value: StatusBadgeTone,
	): 'default' | 'primary' | 'success' | 'warning' | 'error' {
		return value === 'neutral' ? 'default' : value === 'info' ? 'primary' : value;
	}
</script>

<MaterialBadge
	text={label}
	{variant}
	dataTone={tone}
	className={'status-badge ' + className}
/>
