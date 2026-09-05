import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/svelte';
import RecordingIndicator from './RecordingIndicator.svelte';

describe('RecordingIndicator', () => {
	it('uses the shared equalizer in the speaking state', () => {
		const { container } = render(RecordingIndicator, {
			isRecording: true,
			vadState: 'speech',
			duration: 12,
			onCancel: vi.fn(),
		} as any);
		const visual = container.querySelector('.voice-bars') as HTMLElement;

		expect(visual.classList.contains('voice-bars--equalizer')).toBe(true);
		expect(visual.dataset.state).toBe('active');
		expect(visual.dataset.tone).toBe('error');
		expect(visual.querySelectorAll('.voice-bars__bar')).toHaveLength(5);
	});

	it('switches the shared equalizer to processing semantics', () => {
		const { container } = render(RecordingIndicator, {
			processing: true,
			duration: 12,
			onCancel: vi.fn(),
		} as any);
		const visual = container.querySelector('.voice-bars') as HTMLElement;

		expect(visual.dataset.state).toBe('processing');
		expect(visual.dataset.tone).toBe('primary');
	});
});
