<script>
	/**
	 * WorkspaceSurface — shared outer surface for secondary workspaces.
	 *
	 * The surface owns the visual frame only. Each workspace keeps its own
	 * heading, controls and content inside the shared shell.
	 * @prop {import('svelte').Snippet} children — workspace content
	 * @prop {boolean} entering — replay the standard workspace entry motion
	 * @prop {(event: AnimationEvent) => void} onAnimationEnd — entry motion callback
	 */
	let {
		children,
		entering = false,
		onAnimationEnd = () => {},
	} = $props();
</script>

<div
		class="workspace-surface"
		class:workspace-surface--entering={entering}
		onanimationend={(event) => onAnimationEnd?.(event)}
	>
	{@render children?.()}
</div>

<style>
	.workspace-surface {
		display: flex;
		flex-direction: column;
		width: 100%;
		min-width: 0;
		min-height: 100%;
		padding: var(--md-sys-space-2xl);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--md-sys-color-primary) 4%, transparent),
				transparent 220px
			),
			var(--md-sys-color-surface-container-lowest);
		box-shadow: var(--md-sys-elevation-1);
		color: var(--md-sys-color-on-surface);
		transition:
			background-color var(--md-sys-motion-duration-medium)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard);
	}
	.workspace-surface--entering {
		animation: haven-workspace-surface-enter var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-decelerated) both;
	}

	@keyframes haven-workspace-surface-enter {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.workspace-surface--entering {
			animation: none;
		}
	}

	@media (max-width: 640px) {
		.workspace-surface {
			padding: var(--md-sys-space-lg);
		}
	}
</style>
