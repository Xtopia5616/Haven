/**
 * Bridge a Svelte writable store into a `$state` variable. `$state`
 * doesn't track `get(store)` automatically — components must subscribe
 * to receive updates. This helper returns the unsubscribe function so
 * the caller can wire it into `$effect`'s teardown.
 *
 *   let mirror = $state(initial);
 *   $effect(() => syncStore(myStore, (v) => (mirror = v)));
 *
 */
import type { Readable } from 'svelte/store';

export function syncStore<T>(store: Readable<T>, apply: (v: T) => void) {
	return store.subscribe(apply);
}
