import { fireEvent, render } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import WorkspaceSurface from './WorkspaceSurface.svelte';

describe('WorkspaceSurface', () => {
	it('exposes the shared frame and entry state', () => {
		const { container } = render(WorkspaceSurface as any, { entering: true });
		const surface = container.querySelector('.workspace-surface');

		expect(surface).toBeTruthy();
		expect(surface?.classList.contains('workspace-surface--entering')).toBe(true);
	});

	it('forwards the entry animation callback', async () => {
		const onAnimationEnd = vi.fn();
		const { container } = render(WorkspaceSurface as any, { onAnimationEnd });
		const surface = container.querySelector('.workspace-surface');

		await fireEvent.animationEnd(surface!);

		expect(onAnimationEnd).toHaveBeenCalledTimes(1);
	});
});
