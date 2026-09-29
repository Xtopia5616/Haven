import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';
import { svelteTesting } from '@testing-library/svelte/vite';

export default defineConfig({
	plugins: [
		sveltekit(),
		...(Boolean(
			(
				globalThis as typeof globalThis & {
					process?: { env?: Record<string, string | undefined> };
				}
			).process?.env?.VITEST,
		)
			? [svelteTesting()]
			: []),
	],
	server: {
		port: 4721,
		strictPort: true,
	},
	build: {
		target: 'es2022',
	},
	test: {
		include: ['src/**/*.{test,spec}.ts'],
		environment: 'jsdom',
		setupFiles: ['src/test-setup.ts'],
		coverage: {
			provider: 'v8',
			reporter: ['text', 'html'],
			include: ['src/**'],
			exclude: ['src/**/*.{test,spec}.ts', 'src/test-setup.ts'],
		},
	},
});
