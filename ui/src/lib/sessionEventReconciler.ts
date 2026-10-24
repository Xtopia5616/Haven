export interface SessionEventReconcilerDependencies {
	getActiveSessionId: () => string | null;
	isVisible: () => boolean;
	reconcile: (sessionId: string) => Promise<void>;
	intervalMs?: number;
}

/** Periodically compare the visible session's durable cursor while the page is visible. */
export function createSessionEventReconciler(dependencies: SessionEventReconcilerDependencies) {
	const intervalMs = dependencies.intervalMs ?? 5_000;
	let timer: number | null = null;
	let disposed = false;
	let inFlight = false;

	async function reconcileNow(): Promise<void> {
		if (disposed || inFlight || !dependencies.isVisible()) return;
		const sessionId = dependencies.getActiveSessionId();
		if (!sessionId) return;
		inFlight = true;
		try {
			await dependencies.reconcile(sessionId);
		} finally {
			inFlight = false;
		}
	}

	const onFocus = () => void reconcileNow();
	const onVisibilityChange = () => {
		if (dependencies.isVisible()) void reconcileNow();
	};

	function start(): void {
		if (timer !== null || disposed || typeof window === 'undefined') return;
		timer = window.setInterval(() => void reconcileNow(), intervalMs);
		window.addEventListener('focus', onFocus);
		document.addEventListener('visibilitychange', onVisibilityChange);
	}

	function dispose(): void {
		if (disposed) return;
		disposed = true;
		if (timer !== null) window.clearInterval(timer);
		timer = null;
		if (typeof window !== 'undefined') window.removeEventListener('focus', onFocus);
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	}

	return { start, dispose, reconcileNow };
}
