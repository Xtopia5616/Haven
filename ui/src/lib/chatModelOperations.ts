import type { ErrorReportOptions } from './errorHandling.ts';
import type { NotificationType } from './notificationStore.ts';
import type {
	SetReasoningEffortRequest,
	SetWebSearchRequest,
	SwitchModelRequest,
} from './contracts/commands.ts';
import type {
	ReasoningEffortSelectionInput,
	WebSearchModeInput,
} from './contracts/generatedCommands.ts';

export interface ChatModelOption {
	/** Stable id of a configured ModelConfig, used by RequestPolicy.primary. */
	id: string;
	name: string;
	providerName: string;
	model: string;
	reasoningEffort: string;
	webSearch: string;
	apiStyle: string;
	webSearchSupported: boolean;
}

export interface ChatModelOperationsInvoke {
	(command: 'switch_model', payload: SwitchModelRequest): Promise<void>;
	(command: 'set_reasoning_effort', payload: SetReasoningEffortRequest): Promise<void>;
	(command: 'set_web_search', payload: SetWebSearchRequest): Promise<void>;
}

export interface ChatModelOperationsDependencies {
	invoke: ChatModelOperationsInvoke;
	setSkipNextDefaultModelRefresh: (skip: boolean) => void;
	setCurrentModelId: (value: string) => void;
	setCurrentModelName: (value: string) => void;
	setCurrentEffort: (value: string) => void;
	setCurrentWebSearch: (value: string) => void;
	setCurrentApiStyle: (value: string) => void;
	setWebSearchSupported: (value: boolean) => void;
	getEffortLabel: (value: string) => string;
	getWebSearchLabel: (value: string) => string;
	isWebSearchSupported: () => boolean;
	closeModelMenu: () => void;
	notify: (message: string, type: NotificationType, duration: number) => void;
	reportError: (error: unknown, options: ErrorReportOptions) => unknown;
}

/** Own the async operations behind the chat page's model toolbar. */
export function createChatModelOperations(dependencies: ChatModelOperationsDependencies) {
	async function runOperation(
		invokeCommand: () => Promise<void>,
		applySuccess: () => void,
		errorMessage: string,
	): Promise<void> {
		dependencies.setSkipNextDefaultModelRefresh(true);
		try {
			await invokeCommand();
			applySuccess();
		} catch (error) {
			dependencies.setSkipNextDefaultModelRefresh(false);
			dependencies.reportError(error, {
				context: '+page',
				message: errorMessage,
				log: false,
			});
		}
	}

	function selectModel(model: ChatModelOption): Promise<void> {
		return runOperation(
			() => dependencies.invoke('switch_model', { requestKind: 'chat', modelId: model.id }),
			() => {
				dependencies.closeModelMenu();
				let webSearch = model.webSearch;
				let normalizedWebSearch: WebSearchModeInput | null = null;
				if (!model.webSearchSupported && webSearch !== 'off') {
					webSearch = 'off';
					normalizedWebSearch = 'off';
				} else if (model.apiStyle === 'gemini' && webSearch === 'always') {
					webSearch = 'auto';
					normalizedWebSearch = 'auto';
				}
				dependencies.setCurrentModelId(model.id);
				dependencies.setCurrentModelName(model.name);
				dependencies.setCurrentEffort(model.reasoningEffort);
				dependencies.setCurrentWebSearch(webSearch);
				dependencies.setCurrentApiStyle(model.apiStyle);
				dependencies.setWebSearchSupported(model.webSearchSupported);
				dependencies.notify(`已切换对话模型：${model.name}`, 'success', 3000);
				if (normalizedWebSearch) {
					dependencies
						.invoke('set_web_search', { requestKind: 'chat', mode: normalizedWebSearch })
						.catch((error) =>
							dependencies.reportError(error, {
								context: '+page',
								message: '同步对话模型联网搜索设置失败',
								log: false,
								notify: false,
							}),
						);
				}
			},
			'切换模型失败',
		);
	}

	function selectEffort(value: ReasoningEffortSelectionInput | ''): Promise<void> {
		const label = dependencies.getEffortLabel(value) || '默认';
		return runOperation(
			() =>
				dependencies.invoke('set_reasoning_effort', {
					requestKind: 'chat',
					effort: value || null,
				}),
			() => {
				dependencies.setCurrentEffort(value || '');
				dependencies.notify(`思考强度: ${label}`, 'success', 2500);
			},
			'设置思考强度失败',
		);
	}

	function selectWebSearch(value: WebSearchModeInput): Promise<void> {
		if (!dependencies.isWebSearchSupported() && value !== 'off') {
			dependencies.notify('当前模型线协议不支持内置联网搜索', 'info', 3000);
			return Promise.resolve();
		}
		const label = dependencies.getWebSearchLabel(value) || '关闭';
		return runOperation(
			() => dependencies.invoke('set_web_search', { requestKind: 'chat', mode: value }),
			() => {
				dependencies.setCurrentWebSearch(value);
				dependencies.notify(`联网搜索: ${label}`, 'success', 2500);
			},
			'设置联网搜索失败',
		);
	}

	return { selectModel, selectEffort, selectWebSearch };
}
