import type { ErrorReportOptions } from './errorHandling.ts';
import type { NotificationType } from './notificationStore.ts';
import type {
	SetReasoningEffortRequest,
	SetWebSearchRequest,
	SwitchModelRequest,
} from './contracts/commands.ts';

export interface ChatModelOption {
	id: string;
	name?: string | null;
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
		dependencies.closeModelMenu();
		return runOperation(
			() => dependencies.invoke('switch_model', { role: 'chat', modelId: model.id }),
			() => {
				const name = model.name || model.id;
				dependencies.setCurrentModelId(model.id);
				dependencies.setCurrentModelName(name);
				dependencies.notify(`已切换默认模型: ${name}`, 'success', 3000);
			},
			'切换模型失败',
		);
	}

	function selectEffort(value: string): Promise<void> {
		const label = dependencies.getEffortLabel(value) || '默认';
		return runOperation(
			() =>
				dependencies.invoke('set_reasoning_effort', {
					role: 'chat',
					effort: value || null,
				}),
			() => {
				dependencies.setCurrentEffort(value || '');
				dependencies.notify(`思考强度: ${label}`, 'success', 2500);
			},
			'设置思考强度失败',
		);
	}

	function selectWebSearch(value: string): Promise<void> {
		if (!dependencies.isWebSearchSupported() && value !== 'off') {
			dependencies.notify('当前模型线协议不支持内置联网搜索', 'info', 3000);
			return Promise.resolve();
		}
		const label = dependencies.getWebSearchLabel(value) || '关闭';
		return runOperation(
			() => dependencies.invoke('set_web_search', { role: 'chat', mode: value }),
			() => {
				dependencies.setCurrentWebSearch(value);
				dependencies.notify(`联网搜索: ${label}`, 'success', 2500);
			},
			'设置联网搜索失败',
		);
	}

	return { selectModel, selectEffort, selectWebSearch };
}
