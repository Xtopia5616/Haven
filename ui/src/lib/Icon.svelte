<script>
	import { getIconDefinition, getIconTransform } from './icons.ts';

	/**
	 * Shared icon primitive. All icons use the same 24×24 viewBox and inherit
	 * currentColor; only the semantic name and rendered size vary at call sites.
	 * @prop {string} name — key from the shared icon registry
	 * @prop {number|string} size — rendered square size, default 20px
	 * @prop {number|undefined} strokeWidth — optional override for outline icons
	 * @prop {string} className — additional class names
	 * @prop {string} label — accessible label for standalone icons
	 */
	let {
		name = 'help',
		size = 20,
		strokeWidth = undefined,
		className = '',
		label = '',
	} = $props();

	let definition = $derived(getIconDefinition(name));
	let renderedSize = $derived(typeof size === 'number' ? `${size}px` : size);
	let transform = $derived(getIconTransform(definition));
</script>

<svg
	class="icon {className}"
	style={`--icon-size: ${renderedSize}`}
	data-icon={name}
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill={definition.fill}
	stroke={definition.stroke}
	stroke-width={strokeWidth ?? definition.strokeWidth}
	stroke-linecap="round"
	stroke-linejoin="round"
	role={label ? 'img' : undefined}
	aria-label={label || undefined}
	aria-hidden={label ? undefined : 'true'}
>
	{#if transform}
		<g {transform}>{@html definition.body}</g>
	{:else}
		{@html definition.body}
	{/if}
</svg>

<style>
	.icon {
		display: block;
		inline-size: var(--icon-size);
		block-size: var(--icon-size);
		flex: 0 0 auto;
		overflow: visible;
	}
</style>
