<script>
	/**
	 * Shared Haven mark. The shape and fixed brand colors match
	 * assets/branding/haven-mark.svg and the generated app icons.
	 */
	let { size = 24 } = $props();
</script>

<svg
	class="haven-mark"
	viewBox="0 0 64 64"
	width={size}
	height={size}
	fill="none"
	aria-hidden="true"
>
	<path
		d="M18 15h28c5.523 0 10 4.477 10 10v11c0 5.523-4.477 10-10 10H32L21 54v-8h-3c-5.523 0-10-4.477-10-10V25c0-5.523 4.477-10 10-10Z"
		fill="var(--haven-mark-bubble)"
	/>
	<g fill="var(--haven-mark-ink)">
		<rect x="24" y="24.5" width="5" height="12" rx="2.5" />
		<rect x="29.5" y="21.5" width="5" height="18" rx="2.5" />
		<rect x="35" y="24.5" width="5" height="12" rx="2.5" />
	</g>
</svg>
