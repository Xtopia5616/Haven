import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import MemoryView from './MemoryView.svelte';
import type { Fact, FactPage, MemoryFactCursor } from '$lib/contracts/memory.ts';

const {
	goto,
	listFacts,
	page,
	recallMemory,
	registerSessionLifecycleListener,
	reportError,
	searchSessionHistoryFiltered,
} = vi.hoisted(() => {
	let pageValue = { url: new URL('http://localhost/') };
	const subscribers = new Set<(value: typeof pageValue) => void>();
	const page = {
		subscribe(run: (value: typeof pageValue) => void) {
			subscribers.add(run);
			run(pageValue);
			return () => subscribers.delete(run);
		},
		setUrl(href: string) {
			pageValue = { ...pageValue, url: new URL(href, 'http://localhost') };
			for (const subscriber of subscribers) subscriber(pageValue);
		},
	};
	const goto = vi.fn(async (href: string) => page.setUrl(href));
	return {
		goto,
		listFacts: vi.fn(async (): Promise<FactPage> => ({ facts: [], nextCursor: null })),
		page,
		recallMemory: vi.fn(),
		registerSessionLifecycleListener: vi.fn(async () => ({ dispose: vi.fn() })),
		reportError: vi.fn(),
		searchSessionHistoryFiltered: vi.fn(async () => []),
	};
});

vi.mock('$app/stores', () => ({ page }));
vi.mock('$app/navigation', () => ({ goto }));
vi.mock('$lib/memoryCommands.ts', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/memoryCommands.ts')>()),
	listFacts,
	recallMemory,
}));
vi.mock('$lib/sessionCommands.ts', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/sessionCommands.ts')>()),
	searchSessionHistoryFiltered,
}));
vi.mock('$lib/events.ts', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/events.ts')>()),
	registerSessionLifecycleListener,
}));
vi.mock('$lib/errorHandling.ts', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/errorHandling.ts')>()),
	reportError,
}));

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

function recallItem(entityId: string, text: string) {
	return [{ entityId, text, score: 0.9 }];
}

async function openMemoryTab() {
	render(MemoryView);
	await fireEvent.click(screen.getByRole('tab', { name: '长期记忆' }));
	return screen.findByRole('searchbox', { name: '记忆关键词' });
}

async function selectRecallKind(label: string) {
	await fireEvent.click(screen.getByRole('button', { name: '记忆范围' }));
	await fireEvent.click(screen.getByRole('option', { name: label }));
}

async function submitQuery(input: HTMLElement, query: string) {
	await fireEvent.input(input, { target: { value: query } });
	await fireEvent.keyDown(input, { key: 'Enter' });
}

async function flushRecallCompletion() {
	await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

describe('MemoryView recall request lifecycle', () => {
	beforeEach(() => {
		page.setUrl('/');
		goto.mockClear();
		listFacts.mockReset().mockResolvedValue({ facts: [], nextCursor: null });
		recallMemory.mockReset();
		registerSessionLifecycleListener
			.mockReset()
			.mockResolvedValue({ dispose: vi.fn() });
		reportError.mockReset();
		searchSessionHistoryFiltered.mockReset().mockResolvedValue([]);
	});

	it('loads facts by cursor and restarts at the first page when source changes', async () => {
		const cursor: MemoryFactCursor = {
			createdAt: '2026-09-26T00:00:00Z',
			factId: 'fact-00000000000000000000000000000002',
		};
		const firstFact: Fact = {
			factId: 'fact-00000000000000000000000000000001',
			subject: 'user',
			predicate: 'likes',
			object: 'tea',
			source: 'user',
			confidence: 1,
			tags: [],
			created_at: '2026-09-26T00:00:00Z',
			mention_count: 0,
			last_seen_at: null,
			sourceRef: null,
			durability: 1,
		};
		const secondFact = { ...firstFact, factId: cursor.factId, predicate: 'uses', object: 'coffee' };
		listFacts
			.mockResolvedValueOnce({ facts: [firstFact], nextCursor: cursor })
			.mockResolvedValueOnce({ facts: [secondFact], nextCursor: null })
			.mockResolvedValueOnce({ facts: [], nextCursor: null });

		await openMemoryTab();
		await screen.findByText('tea');
		await fireEvent.click(screen.getByRole('button', { name: '加载更多' }));
		await screen.findByText('coffee');
		expect(listFacts).toHaveBeenNthCalledWith(2, {
			source: null,
			cursor,
			limit: 50,
		});

		await fireEvent.click(screen.getByRole('button', { name: '事实来源' }));
		await fireEvent.click(screen.getByRole('option', { name: '推断' }));
		await waitFor(() => expect(listFacts).toHaveBeenCalledTimes(3));
		expect(listFacts).toHaveBeenNthCalledWith(3, {
			source: 'inferred',
			cursor: null,
			limit: 50,
		});
	});

	it('keeps the newest query result and ignores an older request error', async () => {
		const olderRequest = deferred<ReturnType<typeof recallItem>>();
		const latestRequest = deferred<ReturnType<typeof recallItem>>();
		recallMemory
			.mockReturnValueOnce(olderRequest.promise)
			.mockReturnValueOnce(latestRequest.promise);

		const input = await openMemoryTab();
		await selectRecallKind('关于你的事实');
		await submitQuery(input, 'older query');
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(1));
		await submitQuery(input, 'latest query');
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(2));

		latestRequest.resolve(recallItem('fact-00000000000000000000000000000002', 'latest result'));
		await screen.findByText('latest result');
		olderRequest.reject(new Error('stale recall failure'));
		await flushRecallCompletion();

		expect(screen.getByText('latest result')).toBeTruthy();
		expect(screen.queryByText('older result')).toBeNull();
		expect(reportError).not.toHaveBeenCalled();
	});

	it('does not let an old scope request clear the current loading state or results', async () => {
		const factRequest = deferred<ReturnType<typeof recallItem>>();
		const episodeRequest = deferred<ReturnType<typeof recallItem>>();
		recallMemory
			.mockReturnValueOnce(factRequest.promise)
			.mockReturnValueOnce(episodeRequest.promise);

		const input = await openMemoryTab();
		await selectRecallKind('关于你的事实');
		await submitQuery(input, 'same query');
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(1));
		await selectRecallKind('过去的对话');
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(2));

		factRequest.resolve(recallItem('fact-00000000000000000000000000000003', 'stale fact result'));
		await flushRecallCompletion();
		expect(
			screen.getByRole('status', {
				name: '正在检索记忆，正在查找最相关的内容',
			}),
		).toBeTruthy();
		expect(screen.queryByText('stale fact result')).toBeNull();

		episodeRequest.resolve(
			recallItem('msg-00000000000000000000000000000004', 'current episode result'),
		);
		await screen.findByText('current episode result');
		expect(screen.queryByText('stale fact result')).toBeNull();
		expect(
			screen.queryByRole('status', {
				name: '正在检索记忆，正在查找最相关的内容',
			}),
		).toBeNull();
	});

	it('still starts a new request for a repeated identical search', async () => {
		const firstRequest = deferred<ReturnType<typeof recallItem>>();
		const secondRequest = deferred<ReturnType<typeof recallItem>>();
		recallMemory
			.mockReturnValueOnce(firstRequest.promise)
			.mockReturnValueOnce(secondRequest.promise);

		const input = await openMemoryTab();
		await selectRecallKind('关于你的事实');
		await submitQuery(input, 'same query');
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(1));
		await fireEvent.keyDown(input, { key: 'Enter' });
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(2));

		secondRequest.resolve(recallItem('fact-00000000000000000000000000000005', 'second result'));
		await screen.findByText('second result');
		firstRequest.resolve(recallItem('fact-00000000000000000000000000000006', 'first result'));
		await flushRecallCompletion();

		expect(recallMemory).toHaveBeenCalledTimes(2);
		expect(screen.getByText('second result')).toBeTruthy();
		expect(screen.queryByText('first result')).toBeNull();
	});

	it('ignores a pending result after recall is cleared', async () => {
		const pendingRequest = deferred<ReturnType<typeof recallItem>>();
		recallMemory
			.mockReturnValueOnce(
				Promise.resolve(
					recallItem('fact-00000000000000000000000000000007', 'saved result'),
				),
			)
			.mockReturnValueOnce(pendingRequest.promise);

		const input = await openMemoryTab();
		await selectRecallKind('关于你的事实');
		await submitQuery(input, 'same query');
		await screen.findByText('saved result');
		await fireEvent.keyDown(input, { key: 'Enter' });
		await waitFor(() => expect(recallMemory).toHaveBeenCalledTimes(2));

		await fireEvent.click(screen.getByRole('button', { name: '清除' }));
		pendingRequest.resolve(
			recallItem('fact-00000000000000000000000000000008', 'late result'),
		);
		await flushRecallCompletion();

		expect((input as HTMLInputElement).value).toBe('');
		expect(screen.queryByText('saved result')).toBeNull();
		expect(screen.queryByText('late result')).toBeNull();
		expect(
			screen.queryByRole('status', {
				name: '正在检索记忆，正在查找最相关的内容',
			}),
		).toBeNull();
	});
});
