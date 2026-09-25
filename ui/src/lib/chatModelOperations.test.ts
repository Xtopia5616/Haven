import { describe, expect, it } from 'vitest';
import {
	createChatModelOperations,
	type ChatModelOperationsDependencies,
	type ChatModelOperationsInvoke,
} from './chatModelOperations.ts';

function makeHarness(options: {
	failure?: unknown;
	webSearchSupported?: boolean;
} = {}) {
	const calls: Array<{ command: string; payload: unknown }> = [];
	const notifications: Array<{ message: string; type: string; duration: number }> = [];
	const errors: Array<{ error: unknown; options: { context: string; message: string; log?: boolean } }> = [];
	const suppressed: boolean[] = [];
	const modelIds: string[] = [];
	const modelNames: string[] = [];
	const efforts: string[] = [];
	const webSearchModes: string[] = [];
	let modelMenuClosed = 0;

	const invoke = (async (command: string, payload: unknown) => {
		calls.push({ command, payload });
		if (options.failure !== undefined) throw options.failure;
	}) as ChatModelOperationsInvoke;

	const dependencies: ChatModelOperationsDependencies = {
		invoke,
		setSkipNextDefaultModelRefresh: (skip) => suppressed.push(skip),
		setCurrentModelId: (value) => modelIds.push(value),
		setCurrentModelName: (value) => modelNames.push(value),
		setCurrentEffort: (value) => efforts.push(value),
		setCurrentWebSearch: (value) => webSearchModes.push(value),
		getEffortLabel: (value) => (value === 'high' ? '高' : '默认'),
		getWebSearchLabel: (value) => ({ off: '关闭', auto: '自动', always: '总是' })[value] || '',
		isWebSearchSupported: () => options.webSearchSupported ?? true,
		closeModelMenu: () => modelMenuClosed++,
		notify: (message, type, duration) => notifications.push({ message, type, duration }),
		reportError: (error, reportOptions) => errors.push({ error, options: reportOptions }),
	};

	return {
		controller: createChatModelOperations(dependencies),
		calls,
		notifications,
		errors,
		suppressed,
		modelIds,
		modelNames,
		efforts,
		webSearchModes,
		get modelMenuClosed() {
			return modelMenuClosed;
		},
	};
}

describe('createChatModelOperations', () => {
	it('switches the model and applies the page state after the command succeeds', async () => {
		const harness = makeHarness();

		await harness.controller.selectModel({ id: 'provider/model', name: 'Model' });

		expect(harness.calls).toEqual([
			{ command: 'switch_model', payload: { role: 'chat', modelId: 'provider/model' } },
		]);
		expect(harness.modelMenuClosed).toBe(1);
		expect(harness.modelIds).toEqual(['provider/model']);
		expect(harness.modelNames).toEqual(['Model']);
		expect(harness.notifications).toEqual([
			{ message: '已切换默认模型: Model', type: 'success', duration: 3000 },
		]);
		expect(harness.suppressed).toEqual([true]);
	});

	it('sets the reasoning effort and sends null for the default option', async () => {
		const harness = makeHarness();

		await harness.controller.selectEffort('');

		expect(harness.calls).toEqual([
			{ command: 'set_reasoning_effort', payload: { role: 'chat', effort: null } },
		]);
		expect(harness.efforts).toEqual(['']);
		expect(harness.notifications).toEqual([
			{ message: '思考强度: 默认', type: 'success', duration: 2500 },
		]);
		expect(harness.suppressed).toEqual([true]);
	});

	it('sets the web-search mode after the command succeeds', async () => {
		const harness = makeHarness();

		await harness.controller.selectWebSearch('auto');

		expect(harness.calls).toEqual([
			{ command: 'set_web_search', payload: { role: 'chat', mode: 'auto' } },
		]);
		expect(harness.webSearchModes).toEqual(['auto']);
		expect(harness.notifications).toEqual([
			{ message: '联网搜索: 自动', type: 'success', duration: 2500 },
		]);
		expect(harness.suppressed).toEqual([true]);
	});

	it.each([
		{
			name: 'model switch',
			select: (controller: ReturnType<typeof createChatModelOperations>) =>
				controller.selectModel({ id: 'provider/model' }),
			errorMessage: '切换模型失败',
		},
		{
			name: 'reasoning effort',
			select: (controller: ReturnType<typeof createChatModelOperations>) =>
				controller.selectEffort('high'),
			errorMessage: '设置思考强度失败',
		},
		{
			name: 'web search',
			select: (controller: ReturnType<typeof createChatModelOperations>) =>
				controller.selectWebSearch('auto'),
			errorMessage: '设置联网搜索失败',
		},
	])('resets refresh suppression when $name fails', async ({ select, errorMessage }) => {
		const failure = new Error('backend failure');
		const harness = makeHarness({ failure });

		await select(harness.controller);

		expect(harness.suppressed).toEqual([true, false]);
		expect(harness.errors).toEqual([
			{ error: failure, options: { context: '+page', message: errorMessage, log: false } },
		]);
		expect(harness.notifications).toEqual([]);
	});

	it('rejects unsupported non-off web-search modes without invoking or suppressing refresh', async () => {
		const harness = makeHarness({ webSearchSupported: false });

		await harness.controller.selectWebSearch('auto');

		expect(harness.calls).toEqual([]);
		expect(harness.suppressed).toEqual([]);
		expect(harness.webSearchModes).toEqual([]);
		expect(harness.notifications).toEqual([
			{ message: '当前模型线协议不支持内置联网搜索', type: 'info', duration: 3000 },
		]);
	});

	it('still allows turning web search off when the provider does not support it', async () => {
		const harness = makeHarness({ webSearchSupported: false });

		await harness.controller.selectWebSearch('off');

		expect(harness.calls).toEqual([
			{ command: 'set_web_search', payload: { role: 'chat', mode: 'off' } },
		]);
		expect(harness.webSearchModes).toEqual(['off']);
	});
});
