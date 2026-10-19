<script lang="ts">
	import { onMount } from 'svelte';
	import { registerAppListener } from '$lib/events.ts';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialField from '$lib/MaterialField.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { offerMcpDraft } from '$lib/mcpDraftBridge.ts';
	import {
		getMcpPrompt,
		respondMcpFeatureInput,
		cancelMcpFeatureInput,
		listMcpPrompts,
		listMcpResources,
		listMcpResourceTemplates,
		readMcpResource,
		subscribeMcpResource,
		unsubscribeMcpResource,
	} from '$lib/toolsCommands.ts';
	import {
		expandMcpResourceTemplate,
		parseMcpResourceTemplate,
	} from '$lib/mcpResourceTemplates.ts';
	import type { McpServerSnapshotView } from '$lib/contracts/mcp.ts';
	import type {
		McpPromptInfoView,
		McpPromptView,
		McpResourceInfoView,
		McpResourceTemplateView,
		McpResourceReadView,
		McpFeatureCallOutcomeView,
		McpElicitationRequestView,
		McpElicitationFieldView,
		McpElicitationAnswerView,
	} from '$lib/contracts/mcpFeatures.ts';

	interface Props {
		server: McpServerSnapshotView;
		onClose: () => void;
	}

	let { server, onClose }: Props = $props();
	let activeTab = $state<'resources' | 'templates' | 'prompts'>('resources');
	let resources = $state<McpResourceInfoView[]>([]);
	let selectedResourceUri = $state('');
	let resourceRead = $state<McpResourceReadView | null>(null);
	let subscribedResourceUris = $state<string[]>([]);
	let pendingSubscriptionUris = $state<string[]>([]);
	let resourceSubscriptionBusy = $state(false);
	let resourceUpdateAvailable = $state(false);
	let dialogDisposed = false;
	let resourceTemplates = $state<McpResourceTemplateView[]>([]);
	let selectedTemplateUri = $state('');
	let templateArguments = $state<Record<string, string>>({});
	let templateResourceRead = $state<McpResourceReadView | null>(null);
	let expandedTemplateUri = $state('');
	let prompts = $state<McpPromptInfoView[]>([]);
	let selectedPromptName = $state('');
	let promptArguments = $state<Record<string, string>>({});
	let renderedPrompt = $state<McpPromptView | null>(null);
	let loadingResources = $state(false);
	let loadingResource = $state(false);
	let loadingTemplates = $state(false);
	let loadingTemplateResource = $state(false);
	let loadingPrompts = $state(false);
	let renderingPrompt = $state(false);
	let errorMessage = $state('');
	let templateErrorMessage = $state('');
	let pendingFeatureInput = $state<McpFeatureCallOutcomeView | null>(null);
	let pendingFeatureOwner = $state<{
		kind: 'resource' | 'template' | 'prompt';
		identity: string;
	} | null>(null);
	let featureInputValues = $state<Record<string, Record<string, unknown>>>({});
	let submittingFeatureInput = $state(false);
	let stateOnlyRetryTimer: ReturnType<typeof setTimeout> | null = null;

	const targetBinding = $derived(server.targetBinding ?? '');
	const selectedResource = $derived(
		resources.find((resource) => resource.uri === selectedResourceUri) ?? null,
	);
	const selectedResourceSubscribed = $derived(
		subscribedResourceUris.includes(selectedResourceUri),
	);
	const selectedPrompt = $derived(
		prompts.find((prompt) => prompt.name === selectedPromptName) ?? null,
	);
	const selectedTemplate = $derived(
		resourceTemplates.find((template) => template.uriTemplate === selectedTemplateUri) ?? null,
	);
	const resourceText = $derived(resourceDraftText(resourceRead));
	const templateResourceText = $derived(resourceDraftText(templateResourceRead));
	const promptText = $derived(promptDraftText(renderedPrompt));

	onMount(() => {
		dialogDisposed = false;
		activeTab = server.supportsResources ? 'resources' : 'prompts';
		if (!server.enabled || server.status !== 'Connected' || !targetBinding) {
			errorMessage = '服务器当前没有可用的连接，请刷新状态后重试。';
			return;
		}
		let resourceListener: { dispose: () => void } | null = null;
		let statusListener: { dispose: () => void } | null = null;
		void registerAppListener('mcp:resource_changed', (event) => {
			if (event.payload.name !== server.name) return;
			if (event.payload.uri === null) {
				void loadResources(true);
			} else if (event.payload.uri === selectedResourceUri) {
				resourceUpdateAvailable = true;
			}
		}).then((registration) => {
			if (dialogDisposed) registration.dispose();
			else resourceListener = registration;
		});
		void registerAppListener('mcp:status_change', (event) => {
			if (event.payload.name === server.name && event.payload.status !== 'Connected') {
				subscribedResourceUris = [];
				pendingSubscriptionUris = [];
				void cancelPendingFeatureInput();
			}
		}).then((registration) => {
			if (dialogDisposed) registration.dispose();
			else statusListener = registration;
		});
		if (server.supportsResources) void loadResources(false);
		if (server.supportsResources) void loadResourceTemplates(false);
		if (server.supportsPrompts) void loadPrompts(false);
		return () => {
			dialogDisposed = true;
			void cancelPendingFeatureInput();
			resourceListener?.dispose();
			statusListener?.dispose();
			for (const uri of new Set([...subscribedResourceUris, ...pendingSubscriptionUris])) {
				void unsubscribeMcpResource({ name: server.name, targetBinding, uri });
			}
		};
	});

	function closeDialog() {
		dialogDisposed = true;
		void cancelPendingFeatureInput();
		onClose();
	}

	async function loadResources(forceRefresh: boolean) {
		if (!targetBinding || loadingResources) return;
		loadingResources = true;
		errorMessage = '';
		try {
			const result = await listMcpResources({
				name: server.name,
				targetBinding,
				forceRefresh,
			});
			resources = result.resources;
			if (!resources.some((resource) => resource.uri === selectedResourceUri)) {
				selectedResourceUri = '';
				resourceRead = null;
			}
		} catch (error) {
			errorMessage = safeError(error, '加载资源列表失败，连接可能已变化。');
		} finally {
			loadingResources = false;
		}
	}

	async function loadResourceTemplates(forceRefresh: boolean) {
		if (!targetBinding || loadingTemplates) return;
		loadingTemplates = true;
		templateErrorMessage = '';
		try {
			const result = await listMcpResourceTemplates({
				name: server.name,
				targetBinding,
				forceRefresh,
			});
			resourceTemplates = result.resourceTemplates;
			if (
				!resourceTemplates.some((template) => template.uriTemplate === selectedTemplateUri)
			) {
				selectedTemplateUri = '';
				templateArguments = {};
				templateResourceRead = null;
				expandedTemplateUri = '';
			}
		} catch (error) {
			templateErrorMessage = safeError(error, '加载资源模板失败，连接可能已变化。');
		} finally {
			loadingTemplates = false;
		}
	}

	function selectTemplate(template: McpResourceTemplateView) {
		selectedTemplateUri = template.uriTemplate;
		templateArguments = Object.fromEntries(
			parseMcpResourceTemplate(template.uriTemplate).variableNames.map((name) => [name, '']),
		);
		templateResourceRead = null;
		expandedTemplateUri = '';
		templateErrorMessage = '';
	}

	async function readSelectedTemplate() {
		if (!selectedTemplate || loadingTemplateResource) return;
		loadingTemplateResource = true;
		templateErrorMessage = '';
		templateResourceRead = null;
		try {
			expandedTemplateUri = expandMcpResourceTemplate(
				selectedTemplate.uriTemplate,
				Object.fromEntries(
					Object.entries(templateArguments).filter(([, value]) => value !== ''),
				),
			);
			const outcome = await readMcpResource({
				name: server.name,
				targetBinding,
				uri: expandedTemplateUri,
				forceRefresh: false,
			});
			applyFeatureOutcome(outcome, { kind: 'template', identity: expandedTemplateUri });
		} catch (error) {
			templateErrorMessage = safeError(
				error,
				'展开或读取资源模板失败，请检查参数和连接状态。',
			);
		} finally {
			loadingTemplateResource = false;
		}
	}

	async function loadPrompts(forceRefresh: boolean) {
		if (!targetBinding || loadingPrompts) return;
		loadingPrompts = true;
		errorMessage = '';
		try {
			const result = await listMcpPrompts({
				name: server.name,
				targetBinding,
				forceRefresh,
			});
			prompts = result.prompts;
			if (!prompts.some((prompt) => prompt.name === selectedPromptName)) {
				selectedPromptName = '';
				renderedPrompt = null;
			}
		} catch (error) {
			errorMessage = safeError(error, '加载提示词列表失败，连接可能已变化。');
		} finally {
			loadingPrompts = false;
		}
	}

	async function selectResource(resource: McpResourceInfoView, forceRefresh = false) {
		selectedResourceUri = resource.uri;
		resourceRead = null;
		resourceUpdateAvailable = false;
		loadingResource = true;
		errorMessage = '';
		try {
			const outcome = await readMcpResource({
				name: server.name,
				targetBinding,
				uri: resource.uri,
				forceRefresh,
			});
			applyFeatureOutcome(outcome, { kind: 'resource', identity: resource.uri });
		} catch (error) {
			errorMessage = safeError(error, '读取资源失败，连接可能已变化。');
		} finally {
			loadingResource = false;
		}
	}

	async function toggleResourceSubscription() {
		if (!selectedResource || resourceSubscriptionBusy) return;
		const uri = selectedResource.uri;
		resourceSubscriptionBusy = true;
		errorMessage = '';
		try {
			if (subscribedResourceUris.includes(uri)) {
				await unsubscribeMcpResource({ name: server.name, targetBinding, uri });
				subscribedResourceUris = subscribedResourceUris.filter((value) => value !== uri);
				return;
			}
			pendingSubscriptionUris = [...new Set([...pendingSubscriptionUris, uri])];
			const accepted = await subscribeMcpResource({ name: server.name, targetBinding, uri });
			pendingSubscriptionUris = pendingSubscriptionUris.filter((value) => value !== uri);
			if (dialogDisposed) {
				if (accepted)
					await unsubscribeMcpResource({ name: server.name, targetBinding, uri });
				return;
			}
			if (accepted) {
				subscribedResourceUris = [...new Set([...subscribedResourceUris, uri])];
			} else {
				errorMessage = 'MCP 服务器没有确认该资源的更新订阅。';
			}
		} catch (error) {
			pendingSubscriptionUris = pendingSubscriptionUris.filter((value) => value !== uri);
			errorMessage = safeError(error, '订阅资源更新失败，连接可能已变化。');
		} finally {
			resourceSubscriptionBusy = false;
		}
	}

	function selectPrompt(prompt: McpPromptInfoView) {
		selectedPromptName = prompt.name;
		promptArguments = Object.fromEntries(
			prompt.arguments.map((argument) => [argument.name, '']),
		);
		renderedPrompt = null;
		errorMessage = '';
	}

	async function renderSelectedPrompt() {
		if (!selectedPrompt || renderingPrompt) return;
		renderingPrompt = true;
		errorMessage = '';
		try {
			const outcome = await getMcpPrompt({
				name: server.name,
				targetBinding,
				promptName: selectedPrompt.name,
				arguments: { ...promptArguments },
			});
			applyFeatureOutcome(outcome, { kind: 'prompt', identity: selectedPrompt.name });
		} catch (error) {
			errorMessage = safeError(error, '渲染提示词失败，请检查参数和连接状态。');
		} finally {
			renderingPrompt = false;
		}
	}

	function applyFeatureOutcome(
		outcome: McpFeatureCallOutcomeView,
		owner: { kind: 'resource' | 'template' | 'prompt'; identity: string },
	) {
		if (outcome.status === 'input_required') {
			if (dialogDisposed) {
				if (outcome.requestId !== null && targetBinding) {
					void cancelMcpFeatureInput({
						name: server.name,
						targetBinding,
						requestId: outcome.requestId,
					});
				}
				return;
			}
			pendingFeatureInput = outcome;
			pendingFeatureOwner = owner;
			clearStateOnlyRetryTimer();
			featureInputValues = Object.fromEntries(
				outcome.requests.map((request) => [
					request.key,
					Object.fromEntries(
						request.fields
							.filter((field) => field.fieldType === 'boolean')
							.map((field) => [field.name, false]),
					),
				]),
			);
			if (outcome.requests.length === 0 && outcome.requestId !== null) {
				const requestId = outcome.requestId;
				stateOnlyRetryTimer = setTimeout(() => {
					stateOnlyRetryTimer = null;
					if (
						!dialogDisposed &&
						pendingFeatureInput?.requestId === requestId &&
						pendingFeatureInput.requests.length === 0
					) {
						void submitFeatureInput();
					}
				}, 250);
			}
			return;
		}
		clearStateOnlyRetryTimer();
		pendingFeatureInput = null;
		pendingFeatureOwner = null;
		featureInputValues = {};
		if (owner.kind === 'resource' && selectedResourceUri === owner.identity) {
			resourceRead = outcome.resourceRead;
		} else if (owner.kind === 'template' && expandedTemplateUri === owner.identity) {
			templateResourceRead = outcome.resourceRead;
		} else if (owner.kind === 'prompt' && selectedPromptName === owner.identity) {
			renderedPrompt = outcome.prompt;
		}
	}

	function setFeatureInputValue(requestKey: string, fieldName: string, value: unknown) {
		featureInputValues = {
			...featureInputValues,
			[requestKey]: { ...(featureInputValues[requestKey] ?? {}), [fieldName]: value },
		};
	}

	async function submitFeatureInput(decline = false) {
		if (
			!pendingFeatureInput ||
			pendingFeatureInput.requestId === null ||
			submittingFeatureInput
		)
			return;
		clearStateOnlyRetryTimer();
		const requestId = pendingFeatureInput.requestId;
		const owner = pendingFeatureOwner;
		submittingFeatureInput = true;
		errorMessage = '';
		templateErrorMessage = '';
		try {
			const answers: Record<string, McpElicitationAnswerView> = Object.fromEntries(
				pendingFeatureInput.requests.map((request) => [
					request.key,
					{
						action: decline ? 'decline' : 'accept',
						content: decline
							? {}
							: normalizeFeatureInput(request, featureInputValues[request.key] ?? {}),
					},
				]),
			);
			const outcome = await respondMcpFeatureInput({
				name: server.name,
				targetBinding,
				requestId,
				answers,
			});
			if (pendingFeatureInput?.requestId === requestId && owner) {
				applyFeatureOutcome(outcome, owner);
			}
		} catch (error) {
			if (pendingFeatureInput?.requestId !== requestId) return;
			const message = safeError(error, '提交 MCP 输入失败，请检查填写内容和连接状态。');
			if (owner?.kind === 'template') templateErrorMessage = message;
			else errorMessage = message;
		} finally {
			submittingFeatureInput = false;
		}
	}

	function normalizeFeatureInput(
		request: McpElicitationRequestView,
		values: Record<string, unknown>,
	): Record<string, unknown> {
		const normalized: Record<string, unknown> = {};
		for (const field of request.fields) {
			const value = values[field.name];
			if (field.fieldType === 'boolean') {
				if (typeof value === 'boolean') normalized[field.name] = value;
			} else if (typeof value === 'string' && value !== '') {
				normalized[field.name] = field.fieldType === 'string' ? value : Number(value);
			}
		}
		return normalized;
	}

	async function cancelPendingFeatureInput() {
		const requestId = pendingFeatureInput?.requestId;
		clearStateOnlyRetryTimer();
		pendingFeatureInput = null;
		pendingFeatureOwner = null;
		featureInputValues = {};
		if (requestId !== null && requestId !== undefined && targetBinding) {
			try {
				await cancelMcpFeatureInput({ name: server.name, targetBinding, requestId });
			} catch {
				// The selected MCP connection may already have ended.
			}
		}
	}

	function clearStateOnlyRetryTimer() {
		if (stateOnlyRetryTimer !== null) clearTimeout(stateOnlyRetryTimer);
		stateOnlyRetryTimer = null;
	}

	function inputText(field: McpElicitationFieldView, values: Record<string, unknown>): string {
		const value = values[field.name];
		return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
	}

	function insertResourceText() {
		if (!resourceText) return;
		insertIntoChatDraft(resourceText);
	}

	function insertPromptText() {
		if (!promptText) return;
		insertIntoChatDraft(promptText);
	}

	function insertIntoChatDraft(text: string) {
		if (offerMcpDraft(text)) {
			addNotification('内容已加入聊天草稿，请检查后再发送', 'success', 3000);
		} else {
			addNotification('聊天输入框不可用，内容没有加入草稿', 'warning', 3000);
		}
	}

	function resourceDraftText(result: McpResourceReadView | null): string | null {
		if (!result || result.contents.length === 0) return null;
		if (
			result.contents.some(
				(content) => content.text === undefined || content.blob !== undefined,
			)
		) {
			return null;
		}
		return result.contents
			.map(
				(content) =>
					`来源：${content.uri}${content.mimeType ? ` (${content.mimeType})` : ''}\n\n${content.text}`,
			)
			.join('\n\n---\n\n');
	}

	function promptDraftText(result: McpPromptView | null): string | null {
		if (!result || result.messages.length === 0) return null;
		if (
			result.messages.some(
				(message) => message.role !== 'user' || message.content.type !== 'text',
			)
		) {
			return null;
		}
		return result.messages
			.map((message) => (message.content.type === 'text' ? message.content.text : ''))
			.join('\n\n');
	}

	function safeError(error: unknown, fallback: string): string {
		const message = error instanceof Error ? error.message : '';
		return message && message.length <= 240 ? message : fallback;
	}

	function contentLabel(type: string): string {
		return (
			(
				{
					text: '文本',
					image: '图片',
					audio: '音频',
					resource: '嵌入资源',
					resource_link: '资源链接',
				} as Record<string, string>
			)[type] ?? '未知内容'
		);
	}
</script>

<MaterialDialog
	open
	title={`MCP 资源与提示词 · ${server.name}`}
	onClose={closeDialog}
	dialogClass="mcp-feature-dialog"
>
	<div class="feature-tabs" role="tablist" aria-label="MCP 功能">
		{#if server.supportsResources}
			<MaterialButton
				variant={activeTab === 'resources' ? 'tonal' : 'text'}
				label="资源"
				disabled={pendingFeatureInput !== null}
				ariaPressed={activeTab === 'resources'}
				onclick={() => (activeTab = 'resources')}
			/>
			<MaterialButton
				variant={activeTab === 'templates' ? 'tonal' : 'text'}
				label="模板"
				disabled={pendingFeatureInput !== null}
				ariaPressed={activeTab === 'templates'}
				onclick={() => (activeTab = 'templates')}
			/>
		{/if}
		{#if server.supportsPrompts}
			<MaterialButton
				variant={activeTab === 'prompts' ? 'tonal' : 'text'}
				label="提示词"
				disabled={pendingFeatureInput !== null}
				ariaPressed={activeTab === 'prompts'}
				onclick={() => (activeTab = 'prompts')}
			/>
		{/if}
	</div>

	{#if pendingFeatureInput}
		<section class="feature-input" aria-label="MCP 服务器请求的额外输入">
			<h4>服务器需要补充信息（第 {pendingFeatureInput.round} 轮）</h4>
			{#if pendingFeatureInput.requests.length === 0}
				<p>服务器正在处理，无需填写。可以取消本次操作。</p>
			{:else}
				<p>以下字段由 MCP 服务器声明。检查请求内容后再选择提交或拒绝。</p>
			{/if}
			{#each pendingFeatureInput.requests as request, requestIndex (request.key)}
				<fieldset>
					<legend>{request.message}</legend>
					{#each request.fields as field, fieldIndex (field.name)}
						{#if field.fieldType === 'boolean'}
							<label class="feature-checkbox">
								<input
									type="checkbox"
									checked={featureInputValues[request.key]?.[field.name] === true}
									onchange={(event) =>
										setFeatureInputValue(
											request.key,
											field.name,
											event.currentTarget.checked,
										)}
								/>
								<span
									>{field.title || field.name}{field.required
										? ' · 必填'
										: ''}</span
								>
							</label>
							{#if field.description}<small>{field.description}</small>{/if}
						{:else}
							<MaterialField
								label={`${field.title || field.name}${field.required ? ' · 必填' : ''}`}
								forId={`mcp-input-${requestIndex}-${fieldIndex}`}
								hint={field.description ||
									(field.minimum !== null || field.maximum !== null
										? `范围：${field.minimum ?? '无下限'} 至 ${field.maximum ?? '无上限'}`
										: '')}
							>
								{#if field.options.length > 0}
									<select
										id={`mcp-input-${requestIndex}-${fieldIndex}`}
										class="md-input"
										value={inputText(
											field,
											featureInputValues[request.key] ?? {},
										)}
										required={field.required}
										onchange={(event) =>
											setFeatureInputValue(
												request.key,
												field.name,
												event.currentTarget.value,
											)}
									>
										<option value="">请选择</option>
										{#each field.options as option (option)}<option
												value={option}>{option}</option
											>{/each}
									</select>
								{:else}
									<input
										id={`mcp-input-${requestIndex}-${fieldIndex}`}
										class="md-input"
										type={field.fieldType === 'string' ? 'text' : 'number'}
										step={field.fieldType === 'integer' ? '1' : 'any'}
										min={field.fieldType === 'string'
											? undefined
											: (field.minimum ?? undefined)}
										max={field.fieldType === 'string'
											? undefined
											: (field.maximum ?? undefined)}
										minlength={field.fieldType === 'string'
											? (field.minLength ?? undefined)
											: undefined}
										maxlength={field.fieldType === 'string'
											? (field.maxLength ?? 16384)
											: undefined}
										value={inputText(
											field,
											featureInputValues[request.key] ?? {},
										)}
										required={field.required}
										oninput={(event) =>
											setFeatureInputValue(
												request.key,
												field.name,
												event.currentTarget.value,
											)}
									/>
								{/if}
							</MaterialField>
						{/if}
					{/each}
				</fieldset>
			{/each}
			<div class="feature-input-actions">
				{#if pendingFeatureInput.requests.length > 0}
					<MaterialButton
						variant="outlined"
						label="拒绝请求"
						disabled={submittingFeatureInput}
						onclick={() => submitFeatureInput(true)}
					/>
					<MaterialButton
						variant="filled"
						label={submittingFeatureInput ? '提交中…' : '提交回答'}
						disabled={submittingFeatureInput}
						onclick={() => submitFeatureInput(false)}
					/>
				{/if}
				<MaterialButton
					variant="text"
					label="取消本次操作"
					onclick={cancelPendingFeatureInput}
				/>
			</div>
		</section>
	{/if}

	{#if errorMessage}
		<p class="feature-error" role="alert">{errorMessage}</p>
	{/if}

	{#if activeTab === 'resources' && server.supportsResources}
		<div class="feature-actions">
			<p>选择资源后由 Haven 通过该 MCP 连接读取；URI 不会解释为本机文件路径。</p>
			<MaterialButton
				variant="outlined"
				label={loadingResources ? '刷新中…' : '刷新资源列表'}
				disabled={loadingResources || pendingFeatureInput !== null}
				onclick={() => loadResources(true)}
			/>
		</div>
		<div class="feature-list" aria-label="MCP 资源列表">
			{#if loadingResources && resources.length === 0}
				<p>正在读取资源列表…</p>
			{:else if resources.length === 0}
				<p>服务器没有公开可选资源。</p>
			{:else}
				{#each resources as resource (resource.uri)}
					<button
						class="feature-list-item"
						class:selected={selectedResourceUri === resource.uri}
						aria-pressed={selectedResourceUri === resource.uri}
						disabled={pendingFeatureInput !== null}
						onclick={() => selectResource(resource)}
					>
						<strong>{resource.title || resource.name}</strong>
						<span>{resource.uri}</span>
						{#if resource.description}<span>{resource.description}</span>{/if}
						{#if resource.mimeType}<small>{resource.mimeType}</small>{/if}
					</button>
				{/each}
			{/if}
		</div>
		{#if selectedResource}
			<section class="feature-preview" aria-label="资源预览">
				<h4>{selectedResource.title || selectedResource.name}</h4>
				<p class="feature-source">来源：{server.name} · {selectedResource.uri}</p>
				<MaterialButton
					variant="outlined"
					label={loadingResource
						? '读取中…'
						: resourceUpdateAvailable
							? '重新读取已更新资源'
							: '重新读取资源'}
					disabled={loadingResource || pendingFeatureInput !== null}
					onclick={() => selectResource(selectedResource, true)}
				/>
				{#if server.supportsResourceSubscriptions}
					<MaterialButton
						variant={selectedResourceSubscribed ? 'tonal' : 'outlined'}
						label={resourceSubscriptionBusy
							? '处理中…'
							: selectedResourceSubscribed
								? '取消更新订阅'
								: '订阅更新'}
						disabled={resourceSubscriptionBusy ||
							pendingSubscriptionUris.includes(selectedResource.uri) ||
							pendingFeatureInput !== null}
						onclick={toggleResourceSubscription}
					/>
				{/if}
				{#if resourceUpdateAvailable}<p role="status">该资源已更新，可以重新读取。</p>{/if}
				{#if loadingResource}
					<p>正在读取资源…</p>
				{:else if resourceRead}
					{#each resourceRead.contents as content, index (`${content.uri}-${index}`)}
						<div class="resource-content">
							<p>{content.uri}{content.mimeType ? ` · ${content.mimeType}` : ''}</p>
							{#if content.text !== undefined}<pre>{content.text}</pre>{:else}<p>
									二进制内容 · Base64 {content.blob?.length ?? 0} 字符
								</p>{/if}
						</div>
					{/each}
					<MaterialButton
						variant="filled"
						label="将文本加入聊天草稿"
						disabled={!resourceText}
						title={!resourceText ? '该资源包含二进制内容，当前不能加入文本草稿' : ''}
						onclick={insertResourceText}
					/>
				{/if}
			</section>
		{/if}
	{:else if activeTab === 'templates' && server.supportsResources}
		{#if templateErrorMessage}
			<p class="feature-error" role="alert">{templateErrorMessage}</p>
		{/if}
		<div class="feature-actions">
			<p>选择模板并填写 URI 参数后，Haven 才会通过该 MCP 连接读取资源。</p>
			<MaterialButton
				variant="outlined"
				label={loadingTemplates ? '刷新中…' : '刷新模板列表'}
				disabled={loadingTemplates || pendingFeatureInput !== null}
				onclick={() => loadResourceTemplates(true)}
			/>
		</div>
		<div class="feature-list" aria-label="MCP 资源模板列表">
			{#if loadingTemplates && resourceTemplates.length === 0}
				<p>正在读取资源模板…</p>
			{:else if resourceTemplates.length === 0}
				<p>服务器没有公开可选资源模板。</p>
			{:else}
				{#each resourceTemplates as template (template.uriTemplate)}
					<button
						class="feature-list-item"
						class:selected={selectedTemplateUri === template.uriTemplate}
						aria-pressed={selectedTemplateUri === template.uriTemplate}
						disabled={pendingFeatureInput !== null}
						onclick={() => selectTemplate(template)}
					>
						<strong>{template.title || template.name}</strong>
						<span>{template.uriTemplate}</span>
						{#if template.description}<span>{template.description}</span>{/if}
						{#if template.mimeType}<small>{template.mimeType}</small>{/if}
					</button>
				{/each}
			{/if}
		</div>
		{#if selectedTemplate}
			<section class="feature-preview" aria-label="资源模板参数">
				<h4>{selectedTemplate.title || selectedTemplate.name}</h4>
				<p class="feature-source">URI 模板：{selectedTemplate.uriTemplate}</p>
				{#each Object.keys(templateArguments) as variableName (variableName)}
					<MaterialField label={variableName}>
						<input
							class="md-input"
							value={templateArguments[variableName] ?? ''}
							oninput={(event) =>
								(templateArguments[variableName] = event.currentTarget.value)}
						/>
					</MaterialField>
				{/each}
				<MaterialButton
					variant="outlined"
					label={loadingTemplateResource ? '读取中…' : '展开并读取模板'}
					disabled={loadingTemplateResource || pendingFeatureInput !== null}
					onclick={readSelectedTemplate}
				/>
				{#if expandedTemplateUri}<p class="feature-source">
						读取 URI：{expandedTemplateUri}
					</p>{/if}
				{#if loadingTemplateResource}
					<p>正在读取资源…</p>
				{:else if templateResourceRead}
					{#each templateResourceRead.contents as content, index (`${content.uri}-${index}`)}
						<div class="resource-content">
							<p>{content.uri}{content.mimeType ? ` · ${content.mimeType}` : ''}</p>
							{#if content.text !== undefined}<pre>{content.text}</pre>{:else}<p>
									二进制内容 · Base64 {content.blob?.length ?? 0} 字符
								</p>{/if}
						</div>
					{/each}
					<MaterialButton
						variant="filled"
						label="将文本加入聊天草稿"
						disabled={!templateResourceText}
						title={!templateResourceText
							? '该资源包含二进制内容，当前不能加入文本草稿'
							: ''}
						onclick={() =>
							templateResourceText && insertIntoChatDraft(templateResourceText)}
					/>
				{/if}
			</section>
		{/if}
	{:else if activeTab === 'prompts' && server.supportsPrompts}
		<div class="feature-actions">
			<p>提示词由你选择并预览；只有纯文本用户消息可以加入当前聊天草稿。</p>
			<MaterialButton
				variant="outlined"
				label={loadingPrompts ? '刷新中…' : '刷新提示词列表'}
				disabled={loadingPrompts || pendingFeatureInput !== null}
				onclick={() => loadPrompts(true)}
			/>
		</div>
		<div class="feature-list" aria-label="MCP 提示词列表">
			{#if loadingPrompts && prompts.length === 0}
				<p>正在读取提示词列表…</p>
			{:else if prompts.length === 0}
				<p>服务器没有公开可选提示词。</p>
			{:else}
				{#each prompts as prompt (prompt.name)}
					<button
						class="feature-list-item"
						class:selected={selectedPromptName === prompt.name}
						aria-pressed={selectedPromptName === prompt.name}
						disabled={pendingFeatureInput !== null}
						onclick={() => selectPrompt(prompt)}
					>
						<strong>{prompt.title || prompt.name}</strong>
						{#if prompt.description}<span>{prompt.description}</span>{/if}
						<small>{prompt.arguments.length} 个参数</small>
					</button>
				{/each}
			{/if}
		</div>
		{#if selectedPrompt}
			<section class="feature-preview" aria-label="提示词预览">
				<h4>{selectedPrompt.title || selectedPrompt.name}</h4>
				{#each selectedPrompt.arguments as argument (argument.name)}
					<MaterialField
						label={`${argument.name}${argument.required ? ' · 必填' : ''}`}
						hint={argument.description || ''}
					>
						<input
							class="md-input"
							value={promptArguments[argument.name] ?? ''}
							oninput={(event) =>
								(promptArguments[argument.name] = event.currentTarget.value)}
						/>
					</MaterialField>
				{/each}
				<MaterialButton
					variant="outlined"
					label={renderingPrompt ? '生成中…' : '生成并预览'}
					disabled={renderingPrompt || pendingFeatureInput !== null}
					onclick={renderSelectedPrompt}
				/>
				{#if renderedPrompt}
					{#if renderedPrompt.description}<p>{renderedPrompt.description}</p>{/if}
					{#each renderedPrompt.messages as message, index (`${message.role}-${index}`)}
						<div class="prompt-message">
							<strong
								>{message.role === 'user' ? '用户消息' : '助手消息'} · {contentLabel(
									message.content.type,
								)}</strong
							>
							{#if message.content.type === 'text'}
								<pre>{message.content.text}</pre>
							{:else if message.content.type === 'image' || message.content.type === 'audio'}
								<p>{message.content.mimeType} · 二进制内容未解码显示</p>
							{:else if message.content.type === 'resource_link'}
								<p>{message.content.uri}</p>
							{:else}
								<p>
									{message.content.resource.uri} · {message.content.resource
										.mimeType || '资源'}
								</p>
								{#if message.content.resource.text !== undefined}<pre>{message
											.content.resource.text}</pre>{/if}
							{/if}
						</div>
					{/each}
					<MaterialButton
						variant="filled"
						label="将文本加入聊天草稿"
						disabled={!promptText}
						title={!promptText
							? '该提示词包含非用户文本内容，当前不能加入文本草稿'
							: ''}
						onclick={insertPromptText}
					/>
				{/if}
			</section>
		{/if}
	{/if}
</MaterialDialog>

<style>
	:global(.mcp-feature-dialog) {
		width: min(900px, 94vw);
		max-height: 88vh;
		display: flex;
		flex-direction: column;
	}
	.feature-tabs,
	.feature-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
	}
	.feature-tabs {
		justify-content: flex-start;
		padding: var(--md-sys-space-sm) var(--md-sys-space-lg);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.feature-actions {
		padding: var(--md-sys-space-md) var(--md-sys-space-lg) 0;
	}
	.feature-actions p,
	.feature-preview p,
	.feature-list p {
		color: var(--md-sys-color-on-surface-variant);
	}
	.feature-list {
		max-height: 220px;
		overflow: auto;
		display: grid;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
	}
	.feature-list-item {
		display: grid;
		gap: var(--md-sys-space-2xs);
		text-align: left;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
	}
	.feature-list-item.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-secondary-container);
	}
	.feature-list-item span,
	.feature-list-item small,
	.feature-source,
	.resource-content p {
		color: var(--md-sys-color-on-surface-variant);
		overflow-wrap: anywhere;
	}
	.feature-preview {
		max-height: 38vh;
		overflow: auto;
		padding: 0 var(--md-sys-space-lg) var(--md-sys-space-lg);
	}
	.feature-input {
		margin: var(--md-sys-space-md) var(--md-sys-space-lg) 0;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container);
	}
	.feature-input fieldset {
		display: grid;
		gap: var(--md-sys-space-md);
		margin: var(--md-sys-space-md) 0;
		padding: var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
	}
	.feature-input legend,
	.feature-input small {
		color: var(--md-sys-color-on-surface-variant);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.feature-checkbox {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.feature-input-actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
	}
	.resource-content,
	.prompt-message {
		margin: var(--md-sys-space-md) 0;
		padding: var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	pre {
		max-height: 220px;
		overflow: auto;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		font: var(--md-sys-typescale-body-small-font);
		user-select: text;
	}
	.feature-error {
		margin: var(--md-sys-space-md) var(--md-sys-space-lg) 0;
		color: var(--md-sys-color-error);
	}
</style>
