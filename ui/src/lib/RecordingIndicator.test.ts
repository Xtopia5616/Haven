import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
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
		expect(screen.getByRole('status').getAttribute('aria-label')).toBe('正在转写语音');
	});

	it('offers an explicit cancel action while transcription is processing', async () => {
		const onCancel = vi.fn();
		render(RecordingIndicator, { processing: true, duration: 12, onCancel } as any);

		await fireEvent.click(screen.getByRole('button', { name: '取消录音或转写' }));
		expect(onCancel).toHaveBeenCalledTimes(1);
	});
});
