import adapter from '@sveltejs/adapter-static';
import type { Config } from '@sveltejs/kit';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

const config: Config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html',
			precompress: false,
			strict: false,
		}),
		paths: {
			base: '',
		},
		appDir: '_app',
		prerender: {
			entries: ['*'],
		},
	},
};

export default config;
