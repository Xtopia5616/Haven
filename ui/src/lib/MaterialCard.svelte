<script>
	/**
	 * Material Card — shared surface container for grouped content.
	 * @prop {'filled'|'outlined'|'elevated'} variant
	 * @prop {string} className
	 */
	let { variant = 'filled', className = '', children } = $props();
</script>

<div class="md-card md-card--{variant} {className}">
	{@render children?.()}
</div>
