<script lang="ts">
	import { onMount } from 'svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialField from '$lib/MaterialField.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { offerMcpDraft } from '$lib/mcpDraftBridge.ts';
	import {
		getMcpPrompt,
		listMcpPrompts,
		listMcpResources,
		listMcpResourceTemplates,
		readMcpResource,
	} from '$lib/toolsCommands.ts';
	import { expandMcpResourceTemplate, parseMcpResourceTemplate } from '$lib/mcpResourceTemplates.ts';
	import type { McpServerSnapshotView } from '$lib/contracts/mcp.ts';
	import type {
		McpPromptInfoView,
		McpPromptView,
		McpResourceInfoView,
		McpResourceTemplateView,
		McpResourceReadView,
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

	const targetBinding = $derived(server.targetBinding ?? '');
	const selectedResource = $derived(
		resources.find((resource) => resource.uri === selectedResourceUri) ?? null,
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
		activeTab = server.supportsResources ? 'resources' : 'prompts';
		if (!server.enabled || server.status !== 'Connected' || !targetBinding) {
			errorMessage = '服务器当前没有可用的连接，请刷新状态后重试。';
			return;
		}
		if (server.supportsResources) void loadResources(false);
		if (server.supportsResources) void loadResourceTemplates(false);
		if (server.supportsPrompts) void loadPrompts(false);
	});

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
			if (!resourceTemplates.some((template) => template.uriTemplate === selectedTemplateUri)) {
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
			templateResourceRead = await readMcpResource({
				name: server.name,
				targetBinding,
				uri: expandedTemplateUri,
				forceRefresh: false,
			});
		} catch (error) {
			templateErrorMessage = safeError(error, '展开或读取资源模板失败，请检查参数和连接状态。');
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
		loadingResource = true;
		errorMessage = '';
		try {
			resourceRead = await readMcpResource({
				name: server.name,
				targetBinding,
				uri: resource.uri,
				forceRefresh,
			});
		} catch (error) {
			errorMessage = safeError(error, '读取资源失败，连接可能已变化。');
		} finally {
			loadingResource = false;
		}
	}

	function selectPrompt(prompt: McpPromptInfoView) {
		selectedPromptName = prompt.name;
		promptArguments = Object.fromEntries(prompt.arguments.map((argument) => [argument.name, '']));
		renderedPrompt = null;
		errorMessage = '';
	}

	async function renderSelectedPrompt() {
		if (!selectedPrompt || renderingPrompt) return;
		renderingPrompt = true;
		errorMessage = '';
		try {
			renderedPrompt = await getMcpPrompt({
				name: server.name,
				targetBinding,
				promptName: selectedPrompt.name,
				arguments: { ...promptArguments },
			});
		} catch (error) {
			errorMessage = safeError(error, '渲染提示词失败，请检查参数和连接状态。');
		} finally {
			renderingPrompt = false;
		}
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
		if (result.contents.some((content) => content.text === undefined || content.blob !== undefined)) {
			return null;
		}
		return result.contents
			.map((content) => `来源：${content.uri}${content.mimeType ? ` (${content.mimeType})` : ''}\n\n${content.text}`)
			.join('\n\n---\n\n');
	}

	function promptDraftText(result: McpPromptView | null): string | null {
		if (!result || result.messages.length === 0) return null;
		if (result.messages.some((message) => message.role !== 'user' || message.content.type !== 'text')) {
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
		return ({
			text: '文本',
			image: '图片',
			audio: '音频',
			resource: '嵌入资源',
			resource_link: '资源链接',
		} as Record<string, string>)[type] ?? '未知内容';
	}
</script>

<MaterialDialog
	open
	title={`MCP 资源与提示词 · ${server.name}`}
	{onClose}
	dialogClass="mcp-feature-dialog"
>
	<div class="feature-tabs" role="tablist" aria-label="MCP 功能">
		{#if server.supportsResources}
			<MaterialButton
				variant={activeTab === 'resources' ? 'tonal' : 'text'}
				label="资源"
				ariaPressed={activeTab === 'resources'}
				onclick={() => (activeTab = 'resources')}
			/>
			<MaterialButton
				variant={activeTab === 'templates' ? 'tonal' : 'text'}
				label="模板"
				ariaPressed={activeTab === 'templates'}
				onclick={() => (activeTab = 'templates')}
			/>
		{/if}
		{#if server.supportsPrompts}
			<MaterialButton
				variant={activeTab === 'prompts' ? 'tonal' : 'text'}
				label="提示词"
				ariaPressed={activeTab === 'prompts'}
				onclick={() => (activeTab = 'prompts')}
			/>
		{/if}
	</div>

	{#if errorMessage}
		<p class="feature-error" role="alert">{errorMessage}</p>
	{/if}

	{#if activeTab === 'resources' && server.supportsResources}
		<div class="feature-actions">
			<p>选择资源后由 Haven 通过该 MCP 连接读取；URI 不会解释为本机文件路径。</p>
			<MaterialButton
				variant="outlined"
				label={loadingResources ? '刷新中…' : '刷新资源列表'}
				disabled={loadingResources}
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
					label={loadingResource ? '读取中…' : '重新读取资源'}
					disabled={loadingResource}
					onclick={() => selectResource(selectedResource, true)}
				/>
				{#if loadingResource}
					<p>正在读取资源…</p>
				{:else if resourceRead}
					{#each resourceRead.contents as content, index (`${content.uri}-${index}`)}
						<div class="resource-content">
							<p>{content.uri}{content.mimeType ? ` · ${content.mimeType}` : ''}</p>
							{#if content.text !== undefined}<pre>{content.text}</pre>{:else}<p>二进制内容 · Base64 {content.blob?.length ?? 0} 字符</p>{/if}
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
				disabled={loadingTemplates}
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
							oninput={(event) => (templateArguments[variableName] = event.currentTarget.value)}
						/>
					</MaterialField>
				{/each}
				<MaterialButton
					variant="outlined"
					label={loadingTemplateResource ? '读取中…' : '展开并读取模板'}
					disabled={loadingTemplateResource}
					onclick={readSelectedTemplate}
				/>
				{#if expandedTemplateUri}<p class="feature-source">读取 URI：{expandedTemplateUri}</p>{/if}
				{#if loadingTemplateResource}
					<p>正在读取资源…</p>
				{:else if templateResourceRead}
					{#each templateResourceRead.contents as content, index (`${content.uri}-${index}`)}
						<div class="resource-content">
							<p>{content.uri}{content.mimeType ? ` · ${content.mimeType}` : ''}</p>
							{#if content.text !== undefined}<pre>{content.text}</pre>{:else}<p>二进制内容 · Base64 {content.blob?.length ?? 0} 字符</p>{/if}
						</div>
					{/each}
					<MaterialButton
						variant="filled"
						label="将文本加入聊天草稿"
						disabled={!templateResourceText}
						title={!templateResourceText ? '该资源包含二进制内容，当前不能加入文本草稿' : ''}
						onclick={() => templateResourceText && insertIntoChatDraft(templateResourceText)}
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
				disabled={loadingPrompts}
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
							oninput={(event) => (promptArguments[argument.name] = event.currentTarget.value)}
						/>
					</MaterialField>
				{/each}
				<MaterialButton
					variant="outlined"
					label={renderingPrompt ? '生成中…' : '生成并预览'}
					disabled={renderingPrompt}
					onclick={renderSelectedPrompt}
				/>
				{#if renderedPrompt}
					{#if renderedPrompt.description}<p>{renderedPrompt.description}</p>{/if}
					{#each renderedPrompt.messages as message, index (`${message.role}-${index}`)}
						<div class="prompt-message">
							<strong>{message.role === 'user' ? '用户消息' : '助手消息'} · {contentLabel(message.content.type)}</strong>
							{#if message.content.type === 'text'}
								<pre>{message.content.text}</pre>
							{:else if message.content.type === 'image' || message.content.type === 'audio'}
								<p>{message.content.mimeType} · 二进制内容未解码显示</p>
							{:else if message.content.type === 'resource_link'}
								<p>{message.content.uri}</p>
							{:else}
								<p>{message.content.resource.uri} · {message.content.resource.mimeType || '资源'}</p>
								{#if message.content.resource.text !== undefined}<pre>{message.content.resource.text}</pre>{/if}
							{/if}
						</div>
					{/each}
					<MaterialButton
						variant="filled"
						label="将文本加入聊天草稿"
						disabled={!promptText}
						title={!promptText ? '该提示词包含非用户文本内容，当前不能加入文本草稿' : ''}
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
