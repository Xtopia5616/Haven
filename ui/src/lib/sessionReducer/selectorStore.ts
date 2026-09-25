import type { Readable, Subscriber, Unsubscriber } from 'svelte/store';
import type { SessionReducerState } from './types.ts';

type SelectorInvalidator<T> = (value?: T) => void;

type SelectorSubscriber<T> = {
	run: Subscriber<T>;
	invalidate: SelectorInvalidator<T>;
};

/**
 * Build a lazy, equality-gated projection over the single session reducer
 * store. The selected value is the only cached value; the reducer remains the
 * sole state owner.
 */
export function createEqualityGatedSessionSelectorStore<T>(
	source: Readable<SessionReducerState>,
	select: (state: SessionReducerState) => T,
	equals: (previous: T, next: T) => boolean = Object.is,
): Readable<T> {
	const subscribers = new Set<SelectorSubscriber<T>>();
	let current: T | undefined;
	let hasCurrent = false;
	let sourceUnsubscribe: Unsubscriber | undefined;
	let starting = false;

	const update = (state: SessionReducerState) => {
		const next = select(state);
		if (hasCurrent && equals(current as T, next)) return;

		current = next;
		hasCurrent = true;
		const batch = [...subscribers];
		for (const subscriber of batch) {
			if (subscribers.has(subscriber)) subscriber.invalidate();
		}
		for (const subscriber of batch) {
			if (subscribers.has(subscriber)) subscriber.run(next);
		}
	};

	const stop = () => {
		if (!sourceUnsubscribe) return;
		const unsubscribe = sourceUnsubscribe;
		sourceUnsubscribe = undefined;
		hasCurrent = false;
		current = undefined;
		unsubscribe();
	};

	return {
		subscribe(run: Subscriber<T>, invalidate: SelectorInvalidator<T> = () => {}) {
			const subscriber = { run, invalidate };
			subscribers.add(subscriber);

			if (sourceUnsubscribe) {
				run(current as T);
			} else if (starting) {
				// A selected subscriber may subscribe recursively during the source's
				// synchronous initial notification. Reuse that current selection.
				if (hasCurrent) run(current as T);
			} else {
				starting = true;
				try {
					sourceUnsubscribe = source.subscribe(update);
				} catch (error) {
					subscribers.delete(subscriber);
					hasCurrent = false;
					current = undefined;
					throw error;
				} finally {
					starting = false;
				}
				if (subscribers.size === 0) stop();
			}

			return () => {
				subscribers.delete(subscriber);
				if (subscribers.size === 0) stop();
			};
		},
	};
}
