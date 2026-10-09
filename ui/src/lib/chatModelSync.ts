import { reportError } from '$lib/errorHandling.ts';
import { normalizeApiStyle, supportsBuiltinWebSearch } from '$lib/apiStyle.ts';
import { invoke } from '$lib/tauri.ts';
import { loadSettings } from '$lib/settingsCommands.ts';
import type { SettingsPayload } from '$lib/contracts/settings.ts';
import type { ChatModelOption } from '$lib/chatModelOperations.ts';
import type { WebSearchModeInput } from '$lib/contracts/generatedCommands.ts';

type ModelSyncOptions = {
	isDead: () => boolean;
	setModelOptions: (value: ChatModelOption[]) => void;
	setCurrentModelId: (value: string) => void;
	setCurrentModelName: (value: string) => void;
	setCurrentEffort: (value: string) => void;
	setCurrentWebSearch: (value: string) => void;
	setWebSearchSupported: (value: boolean) => void;
	setCurrentApiStyle: (value: string) => void;
};

function chatModelOptions(settings: SettingsPayload): ChatModelOption[] {
	const providers = new Map(settings.llm.providers.map((provider) => [provider.name, provider]));
	return settings.llm.models.flatMap((model) => {
		const providerName = model.provider_name;
		const provider = providers.get(providerName);
		if (
			!model.capabilities.includes('chat') ||
			!model.model ||
			!provider
		) {
			return [];
		}
		const apiStyle = normalizeApiStyle(provider.api_style || 'openai-chat');
		return [
			{
				id: model.id,
				name: model.id,
				providerName,
				model: model.model,
				reasoningEffort: model.reasoning_effort || '',
				webSearch: model.web_search || 'off',
				apiStyle,
				webSearchSupported: supportsBuiltinWebSearch(apiStyle),
			},
		];
	});
}

/**
 * Synchronize the chat toolbar with the configured Chat route. The menu lists
 * named model configurations, and selecting one changes RequestPolicy.primary
 * instead of editing the provider's model id inside the selected configuration.
 */
export function createChatModelSync(options: ModelSyncOptions) {
	const {
		isDead,
		setModelOptions,
		setCurrentModelId,
		setCurrentModelName,
		setCurrentEffort,
		setCurrentWebSearch,
		setWebSearchSupported,
		setCurrentApiStyle,
	} = options;

	function applyDefaultModelFromSettings(settings: SettingsPayload | null) {
		if (!settings) {
			setModelOptions([]);
			setCurrentModelId('');
			setCurrentModelName('');
			setCurrentEffort('');
			setCurrentWebSearch('off');
			setWebSearchSupported(false);
			setCurrentApiStyle('openai-chat');
			return;
		}

		setModelOptions(chatModelOptions(settings));
		const chatPolicy = settings.llm.request_policies.find((policy) => policy.request === 'chat');
		const selectedModel = settings.llm.models.find((model) => model.id === chatPolicy?.primary);
		const providerName = selectedModel?.provider_name;
		const provider = providerName
			? settings.llm.providers.find((item) => item.name === providerName)
			: undefined;
		const apiStyle = normalizeApiStyle(provider?.api_style || 'openai-chat');
		const webSearchSupported = !!selectedModel && supportsBuiltinWebSearch(apiStyle);
		const storedWebSearch = selectedModel?.web_search || 'off';
		let webSearch = storedWebSearch;
		let normalizedWebSearch: WebSearchModeInput | null = null;
		if (selectedModel && !webSearchSupported && webSearch !== 'off') {
			webSearch = 'off';
			normalizedWebSearch = 'off';
		} else if (selectedModel && apiStyle === 'gemini' && webSearch === 'always') {
			webSearch = 'auto';
			normalizedWebSearch = 'auto';
		}

		setCurrentModelId(selectedModel?.id || '');
		setCurrentModelName(selectedModel?.id || '');
		setCurrentEffort(selectedModel?.reasoning_effort || '');
		setCurrentApiStyle(apiStyle);
		setWebSearchSupported(webSearchSupported);
		setCurrentWebSearch(webSearchSupported ? webSearch : 'off');

		// Normalize stale settings so they cannot become active if the profile or
		// provider wire style changes later.
		if (normalizedWebSearch) {
			invoke('set_web_search', { requestKind: 'chat', mode: normalizedWebSearch }).catch((error) => {
				reportError(error, {
					context: '+page',
					message: '同步对话模型联网搜索设置失败',
					log: false,
					notify: false,
				});
			});
		}
	}

	let syncGeneration = 0;

	/** Re-fetch settings and refresh the toolbar controls after external edits. */
	function refreshDefaultModelFromBackend() {
		const generation = ++syncGeneration;
		loadSettings()
			.then((settings) => {
				if (isDead() || generation !== syncGeneration) return;
				applyDefaultModelFromSettings(settings);
			})
			.catch((error) => {
				reportError(error, {
					context: '+page',
					message: '刷新默认模型失败',
					notify: false,
				});
			});
	}

	return { applyDefaultModelFromSettings, refreshDefaultModelFromBackend };
}
