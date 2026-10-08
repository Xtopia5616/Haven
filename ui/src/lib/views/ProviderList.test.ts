import { describe, expect, it } from 'vitest';
import providerListSource from './ProviderList.svelte?raw';

describe('ProviderList responsive layout', () => {
	it('keeps the provider toolbar within its available content width', () => {
		expect(providerListSource).toMatch(/\.provider-toolbar\s*\{[^}]*flex-wrap:\s*nowrap;/s);
		expect(providerListSource).toMatch(
			/\.provider-toolbar-actions\s*\{[^}]*flex-wrap:\s*nowrap;[^}]*flex:\s*0 1 auto;[^}]*max-width:\s*100%;/s,
		);
		expect(providerListSource).toContain(
			'transform: translateX(calc(0px - var(--md-sys-space-2xl)));',
		);
		expect(providerListSource).toContain('flex: 0 0 auto;');
		expect(providerListSource).toContain('margin-right: var(--md-sys-space-2xl);');
		expect(providerListSource).toContain('flex-direction: column-reverse;');
		expect(providerListSource).toContain('width: 100%;');
		expect(providerListSource).toContain('@container settings-content (max-width: 700px)');
		expect(providerListSource).toContain('@container settings-content (max-width: 455px)');
	});
});
