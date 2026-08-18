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
export function syncStore(store, apply) {
	return store.subscribe(apply);
}

export function syncStoreImmediate(store, apply, getCurrent) {
	if (getCurrent) apply(getCurrent());
	return store.subscribe(apply);
}
