/**
 * Bridge a Svelte writable store into a `$state` variable. `$state`
 * doesn't track `get(store)` automatically — components must subscribe
 * to receive updates. This helper returns the unsubscribe function so
 * the caller can wire it into `$effect`'s teardown.
 *
 *   let mirror = $state(initial);
 *   $effect(() => syncStore(myStore, (v) => (mirror = v)));
 *
 * For convenience, `syncStoreImmediate` also assigns the current value
 * synchronously (some components need to seed from `get(store)` before
 * subscription fires — see sessionMessagesStore mirror).
 */
import type { Readable, Writable } from 'svelte/store';

export function syncStore<T>(store: Readable<T>, apply: (v: T) => void) {
	return store.subscribe(apply);
}

export function syncStoreImmediate<T>(store: Writable<T>, apply: (v: T) => void, getCurrent: () => T) {
	if (getCurrent) apply(getCurrent());
	return store.subscribe(apply);
}
