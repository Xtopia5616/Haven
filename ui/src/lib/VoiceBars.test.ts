import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import VoiceBars from './VoiceBars.svelte';

describe('VoiceBars', () => {
	it('renders the loading float pattern with the requested bar count', () => {
		const { container } = render(VoiceBars, {
			pattern: 'float',
			count: 3,
			tone: 'primary',
		} as any);
		const visual = container.querySelector('.voice-bars') as HTMLElement;

		expect(visual.classList.contains('voice-bars--float')).toBe(true);
		expect(visual.dataset.tone).toBe('primary');
		expect(visual.querySelectorAll('.voice-bars__bar')).toHaveLength(3);
	});

	it('exposes the recording equalizer state and tone', () => {
		const { container } = render(VoiceBars, {
			pattern: 'equalizer',
			count: 5,
			tone: 'error',
			state: 'active',
		} as any);
		const visual = container.querySelector('.voice-bars') as HTMLElement;

		expect(visual.classList.contains('voice-bars--equalizer')).toBe(true);
		expect(visual.dataset.tone).toBe('error');
		expect(visual.dataset.state).toBe('active');
		expect(visual.querySelectorAll('.voice-bars__bar')).toHaveLength(5);
	});
});
