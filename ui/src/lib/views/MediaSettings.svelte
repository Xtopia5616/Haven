<script>
	import { onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification } from '$lib/stores.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialCard from '$lib/MaterialCard.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import ApiKeyDialog from '$lib/ApiKeyDialog.svelte';
	import ApiKeyField from '$lib/ApiKeyField.svelte';
	import { inputFormats } from '$lib/inputFormats.ts';
	import {
		inputElementValue,
		withBooleanValue,
		withEventValue,
		withNumberValue,
		withStringValue,
	} from '$lib/typedCallbacks.js';
	import { mediaCapabilityBackend, sttCapabilityBackend } from '$lib/apiStyle.ts';

	/** Media configuration owns the channel-specific UI and discovery state.
	 * The settings page still owns the shared mutable config and persistence. */
	let {
		llmConfig,
		audio,
		stt,
		ocr,
		tts,
		imageGen,
		contextLimits,
		keyConfigured,
		keyConfiguredProviders = {},
		mcpServerNames = [],
	} = $props();

	const STT_SPECIAL = new Set(['llm', 'mcp', 'none']);
	const OCR_PROVIDER_OPTIONS = [
		{ value: 'llm', label: '视觉模型 (Image Model)' },
		{ value: 'baidu', label: 'Baidu 通用文字识别' },
		{ value: 'azure', label: 'Azure AI Vision' },
		{ value: 'tencent', label: 'Tencent 通用印刷体' },
		{ value: 'none', label: '未配置（透传图片）' },
	];

	/** @param {string} name */
	function providerByName(name) {
		return (llmConfig.providers || []).find(
			(/** @type {any} */ provider) => provider.name === name,
		);
	}
	/** @param {string} current */
	function mediaProviderOptions(current) {
		const options = [{ value: 'none', label: '未配置' }];
		for (const provider of /** @type {any[]} */ (llmConfig.providers || []))
			if (provider?.name) options.push({ value: provider.name, label: provider.name });
		if (current && current !== 'none' && !options.some((option) => option.value === current))
			options.push({ value: current, label: `${current}（需重选「模型」页 Provider）` });
		return options;
	}
	/** @param {string} name @param {'tts' | 'image_gen'} capability */
	function mediaProviderKind(name, capability) {
		if (!name || name === 'none') return '';
		const provider = providerByName(name);
		return provider ? mediaCapabilityBackend(provider, capability) : '';
	}
	function sttProviderOptions() {
		const options = [
			{ value: 'llm', label: '音频模型 (Audio Model)' },
			{ value: 'mcp', label: 'MCP Server' },
			{ value: 'none', label: '未配置' },
		];
		for (const provider of /** @type {any[]} */ (llmConfig.providers || []))
			if (provider?.name) options.push({ value: provider.name, label: provider.name });
		if (
			stt.provider &&
			!STT_SPECIAL.has(stt.provider) &&
			!options.some((option) => option.value === stt.provider)
		)
			options.push({
				value: stt.provider,
				label: `${stt.provider}（需重选「模型」页 Provider）`,
			});
		return options;
	}
	/** @param {string} name */
	function sttBackendKind(name) {
		if (!name || STT_SPECIAL.has(name)) return '';
		const provider = providerByName(name);
		return provider ? sttCapabilityBackend(provider) : '';
	}
	/** @param {string} provider */
	function isNamedSttProvider(provider) {
		return !!provider && !STT_SPECIAL.has(provider);
	}
	/** @param {string} kind */
	function sttModelPlaceholder(kind) {
		if (kind === 'deepgram') return 'nova-3';
		if (kind === 'assemblyai') return 'assemblyai_default';
		if (kind === 'groq') return 'whisper-large-v3-turbo';
		if (kind === 'gemini') return 'gemini-2.5-flash';
		return 'whisper-1';
	}
	/** @type {any[]} */
	let sttModels = $state([]);
	let sttFetching = $state(false);
	/** @type {ReturnType<typeof setTimeout> | undefined} */
	let sttFetchTimer = undefined;
	/** @param {string} kind */
	function sttModelOptions(kind) {
		if (kind === 'deepgram')
			return [
				{ value: 'nova-3', label: 'nova-3' },
				{ value: 'nova-2', label: 'nova-2' },
				{ value: 'whisper-large-v3', label: 'whisper-large-v3' },
				{ value: 'whisper-large-v3-turbo', label: 'whisper-large-v3-turbo' },
			];
		if (kind === 'assemblyai')
			return [
				{ value: 'assemblyai_default', label: 'AssemblyAI Default' },
				{ value: 'universal', label: 'universal' },
				{ value: 'universal-2', label: 'universal-2' },
				{ value: 'universal-3-pro', label: 'universal-3-pro' },
			];
		return (sttModels || []).map((model) => ({
			value: model.id,
			label: model.name || model.id,
		}));
	}
	async function fetchSttModels() {
		const name = stt.provider;
		if (!isNamedSttProvider(name)) return;
		const kind = sttBackendKind(name);
		if (!kind || kind === 'deepgram' || kind === 'assemblyai') return;
		const provider = providerByName(name);
		const base = (provider?.base_url || '').trim();
		const key = provider?.api_key || '';
		if (!base || (!key && !keyConfiguredProviders[name] && !keyConfigured.stt)) {
			sttModels = [];
			return;
		}
		sttFetching = true;
		try {
			sttModels =
				(await invoke('discover_models', {
					baseUrl: base,
					apiKey: key,
					provider: name,
					role: 'stt',
				})) || [];
		} catch (e) {
			sttModels = [];
			reportError(e, { context: 'MediaSettings', message: '获取 STT 模型失败', log: false });
		} finally {
			sttFetching = false;
		}
	}
	function scheduleSttFetch() {
		clearTimeout(sttFetchTimer);
		sttFetchTimer = setTimeout(fetchSttModels, 500);
	}

	let keyDlg = $state({ open: false, model: '', label: '' });
	/** @param {string} model @param {string} label */
	function openKeyDialog(model, label) {
		keyDlg = { open: true, model, label };
	}
	/** @param {string} value */
	function confirmMediaKey(value) {
		if (keyDlg.model === 'ocr') ocr.api_key = value;
		else if (keyDlg.model === 'ocr_secret') ocr.api_secret = value;
		keyConfigured[keyDlg.model] = true;
		keyDlg = { open: false, model: '', label: '' };
	}
	/** @param {string} value */
	function setSttProvider(value) {
		stt.provider = value;
		if (!isNamedSttProvider(value)) sttModels = [];
	}
	onDestroy(() => clearTimeout(sttFetchTimer));
</script>

<SettingsSection ariaLabel="媒体" className="media-section">
	<p class="model-hint">
		按模态配置输入与输出。STT / OCR 可走专用通道或「模型」页的 Audio / Image Model；TTS /
		文生图复用「模型」页已添加的 Provider（Base URL + API Key）。
	</p>
	<div class="card-list">
		{#each inputFormats as format (format.id)}
			<MaterialCard variant="outlined" className="settings-card">
				<div class="card-head">
					<span class="card-title">{format.label}</span>
					<p class="card-hint">{format.hint}</p>
				</div>
				{#if format.id === 'voice'}
					<div class="capability-block first">
						<h4>输入 · 采集</h4>
						<div class="form-row switch-row">
							<span class="switch-label">录音转写使用专用音频模型</span
							><MaterialSwitch
								checked={llmConfig.stt_use_audio_model}
								ariaLabel="切换专用音频模型"
								onChange={withBooleanValue((v) => {
									llmConfig.stt_use_audio_model = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="audio-sample-rate">Sample Rate</label><MaterialNumberField
								id="audio-sample-rate"
								value={audio.sample_rate}
								onChange={withNumberValue((v) => {
									audio.sample_rate = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="audio-channels">Channels</label><MaterialNumberField
								id="audio-channels"
								value={audio.channels}
								min={1}
								max={2}
								onChange={withNumberValue((v) => {
									audio.channels = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="audio-max-duration">Max Duration (sec)</label
							><MaterialNumberField
								id="audio-max-duration"
								value={audio.max_duration_secs}
								min={10}
								max={300}
								onChange={withNumberValue((v) => {
									audio.max_duration_secs = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="audio-silence-timeout">Silence Timeout (ms)</label
							><MaterialNumberField
								id="audio-silence-timeout"
								value={audio.silence_timeout_ms}
								min={500}
								max={10000}
								step={100}
								onChange={withNumberValue((v) => {
									audio.silence_timeout_ms = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="audio-vad-threshold">VAD Threshold</label><input
								id="audio-vad-threshold"
								type="range"
								class="md-slider"
								value={audio.vad_threshold}
								min="0"
								max="1"
								step="0.05"
								style="--vad-fill: {audio.vad_threshold * 100}%"
								oninput={withEventValue((e) => {
									audio.vad_threshold = Number(inputElementValue(e));
								})}
							/><span class="range-value">{audio.vad_threshold}</span>
						</div>
					</div>
					<div class="capability-block">
						<h4>输入 · 语音转写（STT）</h4>
						<p class="model-hint">
							推荐：「模型」页 Audio Model 选 Whisper / Gemini 等，此处 Provider
							选「音频模型」。也可选已配置 Provider 或 MCP。
						</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">STT Provider</span><MaterialSelect
									id="voice-stt-provider"
									value={stt.provider}
									options={sttProviderOptions()}
									onChange={setSttProvider}
								/>
							</div>
							{#if stt.provider === 'mcp'}
								<div class="model-field">
									<span class="field-label">MCP Server</span><MaterialAutocomplete
										id="voice-stt-mcp"
										value={stt.mcp_server}
										options={mcpServerNames.map((name) => ({
											value: name,
											label: name,
										}))}
										placeholder="Pick a configured MCP server"
										loading={false}
										onChange={withStringValue((v) => {
											stt.mcp_server = v;
										})}
									/>
								</div>
							{:else if isNamedSttProvider(stt.provider)}
								{#if sttBackendKind(stt.provider)}
									<div class="model-field">
										<span class="field-label">Model</span><MaterialAutocomplete
											id="voice-stt-model"
											value={stt.model}
											options={sttModelOptions(sttBackendKind(stt.provider))}
											placeholder={sttModelPlaceholder(
												sttBackendKind(stt.provider),
											)}
											loading={sttFetching}
											onChange={withStringValue((v) => {
												stt.model = v;
											})}
											onFocus={scheduleSttFetch}
										/>
									</div>
								{:else}<p class="model-hint">
										该 Provider 不支持 STT（需 OpenAI 兼容 / Gemini / Deepgram /
										AssemblyAI）。
									</p>{/if}
							{/if}
							{#if stt.provider !== 'none'}
								<div class="model-field">
									<span class="field-label">Timeout (sec)</span
									><MaterialNumberField
										id="voice-stt-timeout"
										value={stt.timeout_secs}
										min={5}
										max={600}
										onChange={withNumberValue((v) => {
											stt.timeout_secs = v;
										})}
									/>
								</div>
								<div class="model-field">
									<span class="field-label">Min Confidence</span><input
										id="voice-stt-min-confidence"
										type="range"
										class="md-slider"
										value={stt.min_confidence}
										min="0"
										max="1"
										step="0.05"
										style="--vad-fill: {stt.min_confidence * 100}%"
										oninput={withEventValue((e) => {
											stt.min_confidence = Number(inputElementValue(e));
										})}
									/><span class="range-value">{stt.min_confidence}</span>
								</div>
							{/if}
						</div>
						<p class="model-hint">
							置信度低于阈值时回落主模型。仅 Deepgram / AssemblyAI / MCP
							报告置信度；Whisper 等在失败或空结果时回落。
						</p>
					</div>
					<div class="capability-block">
						<h4>输出 · 语音合成（TTS）</h4>
						<p class="model-hint">
							「朗读这段话」「读出来」等请求合成语音并附到消息；选「模型」页已添加的
							Provider。
						</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">Provider</span><MaterialSelect
									id="tts-provider"
									value={tts.provider}
									options={mediaProviderOptions(tts.provider)}
									onChange={withStringValue((v) => {
										tts.provider = v;
									})}
								/>
							</div>
							{#if tts.provider !== 'none'}
								{#if mediaProviderKind(tts.provider, 'tts') === 'elevenlabs'}<div
										class="model-field"
									>
										<span class="field-label">Voice ID</span><input
											id="tts-voice"
											type="text"
											class="md-input"
											bind:value={tts.voice}
											placeholder="elevenlabs voice id"
											autocomplete="off"
										/>
									</div>
								{:else if mediaProviderKind(tts.provider, 'tts') === 'openai'}<div
										class="model-field"
									>
										<span class="field-label">Model</span><input
											id="tts-model"
											type="text"
											class="md-input"
											bind:value={tts.model}
											placeholder="tts-1 / gpt-4o-mini-tts"
											autocomplete="off"
										/>
									</div>
									<div class="model-field">
										<span class="field-label">Voice</span><input
											id="tts-voice"
											type="text"
											class="md-input"
											bind:value={tts.voice}
											placeholder="alloy / nova / echo…"
											autocomplete="off"
										/>
									</div>
								{:else}<p class="model-hint">
										该 Provider 不支持 TTS（需 OpenAI 兼容）。
									</p>{/if}
								<div class="model-field">
									<span class="field-label">Timeout (sec)</span
									><MaterialNumberField
										id="tts-timeout"
										value={tts.timeout_secs}
										min={5}
										max={300}
										onChange={withNumberValue((v) => {
											tts.timeout_secs = v;
										})}
									/>
								</div>
							{/if}
						</div>
					</div>
				{:else if format.id === 'image'}
					<div class="capability-block first">
						<h4>输入 · 附件与理解</h4>
						<p class="model-hint">
							当前压缩：最长边 ≤{contextLimits.max_attachment_image_dim_px}px、质量 {Math.round(
								contextLimits.attachment_image_jpeg_quality * 100,
							)}%。
						</p>
						<div class="form-row switch-row">
							<span class="switch-label">图片理解使用专用视觉模型</span
							><MaterialSwitch
								checked={llmConfig.vision_use_image_model}
								ariaLabel="切换专用视觉模型"
								onChange={withBooleanValue((v) => {
									llmConfig.vision_use_image_model = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="max-attachment-images">单条消息最多图片数</label
							><MaterialNumberField
								id="max-attachment-images"
								value={contextLimits.max_attachment_images}
								min={1}
								max={20}
								step={1}
								onChange={withNumberValue((v) => {
									contextLimits.max_attachment_images = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="max-attachment-image-mb">单张图片大小上限 (MiB)</label
							><MaterialNumberField
								id="max-attachment-image-mb"
								value={Math.round(
									(contextLimits.max_attachment_image_bytes / 1048576) * 10,
								) / 10}
								min={1}
								max={50}
								step={1}
								onChange={withNumberValue((v) => {
									contextLimits.max_attachment_image_bytes = Math.round(
										v * 1024 * 1024,
									);
								})}
							/>
						</div>
						<div class="form-row">
							<label for="max-attachment-image-dim">压缩最长边 (px)</label
							><MaterialNumberField
								id="max-attachment-image-dim"
								value={contextLimits.max_attachment_image_dim_px}
								min={512}
								max={4096}
								step={64}
								onChange={withNumberValue((v) => {
									contextLimits.max_attachment_image_dim_px = v;
								})}
							/>
						</div>
						<div class="form-row">
							<label for="attachment-image-quality">JPEG 压缩质量</label
							><MaterialNumberField
								id="attachment-image-quality"
								value={contextLimits.attachment_image_jpeg_quality}
								min={0.1}
								max={1}
								step={0.05}
								onChange={withNumberValue((v) => {
									contextLimits.attachment_image_jpeg_quality = v;
								})}
							/>
						</div>
					</div>
					<div class="capability-block">
						<h4>输入 · 文字提取（OCR）</h4>
						<p class="model-hint">
							「提取文字」意图走 OCR；推荐选视觉模型。专用云 OCR
							失败或低置信度时回落到 Image Model。
						</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">OCR Provider</span><MaterialSelect
									id="img-ocr-provider"
									value={ocr.provider}
									options={OCR_PROVIDER_OPTIONS}
									onChange={withStringValue((v) => {
										ocr.provider = v;
									})}
								/>
							</div>
							{#if ocr.provider === 'baidu' || ocr.provider === 'tencent' || ocr.provider === 'azure'}<div
									class="model-field"
								>
									<span class="field-label">API Key</span><ApiKeyField
										id="img-ocr-api-key"
										configured={keyConfigured.ocr}
										onEdit={() => openKeyDialog('ocr', 'OCR API Key')}
									/>
								</div>{/if}
							{#if ocr.provider === 'baidu' || ocr.provider === 'tencent'}<div
									class="model-field"
								>
									<span class="field-label">Secret Key</span><ApiKeyField
										id="img-ocr-secret"
										configured={keyConfigured.ocr_secret}
										onEdit={() => openKeyDialog('ocr_secret', 'OCR Secret Key')}
									/>
								</div>{/if}
							{#if ocr.provider === 'azure'}<div class="model-field">
									<span class="field-label">Base URL</span><input
										id="img-ocr-base-url"
										type="text"
										class="md-input"
										bind:value={ocr.base_url}
										placeholder="https://&lt;resource&gt;.cognitiveservices.azure.com"
										autocomplete="off"
									/>
								</div>{/if}
							{#if ocr.provider !== 'none'}<div class="model-field">
									<span class="field-label">Timeout (sec)</span
									><MaterialNumberField
										id="img-ocr-timeout"
										value={ocr.timeout_secs}
										min={5}
										max={300}
										onChange={withNumberValue((v) => {
											ocr.timeout_secs = v;
										})}
									/>
								</div>
								<div class="model-field">
									<span class="field-label">Min Confidence</span><input
										id="img-ocr-min-confidence"
										type="range"
										class="md-slider"
										value={ocr.min_confidence}
										min="0"
										max="1"
										step="0.05"
										style="--vad-fill: {ocr.min_confidence * 100}%"
										oninput={withEventValue((e) => {
											ocr.min_confidence = Number(inputElementValue(e));
										})}
									/><span class="range-value">{ocr.min_confidence}</span>
								</div>{/if}
						</div>
					</div>
					<div class="capability-block">
						<h4>输出 · 文生图</h4>
						<p class="model-hint">
							「画一只猫」「生成海报」等请求生成图片并附到消息；需 OpenAI 兼容或
							Gemini Provider。
						</p>
						<div class="stt-grid">
							<div class="model-field">
								<span class="field-label">Provider</span><MaterialSelect
									id="ig-provider"
									value={imageGen.provider}
									options={mediaProviderOptions(imageGen.provider)}
									onChange={withStringValue((v) => {
										imageGen.provider = v;
									})}
								/>
							</div>
							{#if imageGen.provider !== 'none'}{#if mediaProviderKind(imageGen.provider, 'image_gen')}<div
										class="model-field"
									>
										<span class="field-label">Model</span><input
											id="ig-model"
											type="text"
											class="md-input"
											bind:value={imageGen.model}
											placeholder={mediaProviderKind(
												imageGen.provider,
												'image_gen',
											) === 'gemini'
												? 'gemini-2.5-flash-image'
												: 'gpt-image-1'}
											autocomplete="off"
										/>
									</div>
									<div class="model-field">
										<span class="field-label">Timeout (sec)</span
										><MaterialNumberField
											id="ig-timeout"
											value={imageGen.timeout_secs}
											min={10}
											max={600}
											onChange={withNumberValue((v) => {
												imageGen.timeout_secs = v;
											})}
										/>
									</div>{:else}<p class="model-hint">
										该 Provider 不支持文生图（需 OpenAI 兼容或 Gemini）。
									</p>{/if}{/if}
						</div>
					</div>
				{:else if format.id === 'file'}
					<div class="form-row">
						<label for="max-attachment-files">单条消息最多文件数</label
						><MaterialNumberField
							id="max-attachment-files"
							value={contextLimits.max_attachment_files}
							min={1}
							max={20}
							step={1}
							onChange={withNumberValue((v) => {
								contextLimits.max_attachment_files = v;
							})}
						/>
					</div>
					<div class="form-row">
						<label for="max-attachment-file-mb">单个文件大小上限 (MiB)</label
						><MaterialNumberField
							id="max-attachment-file-mb"
							value={Math.round(
								(contextLimits.max_attachment_file_bytes / 1048576) * 10,
							) / 10}
							min={1}
							max={100}
							step={1}
							onChange={withNumberValue((v) => {
								contextLimits.max_attachment_file_bytes = Math.round(
									v * 1024 * 1024,
								);
							})}
						/>
					</div>
				{/if}
			</MaterialCard>
		{/each}
	</div>
</SettingsSection>

<ApiKeyDialog
	open={keyDlg.open}
	label={keyDlg.label}
	configured={keyDlg.model ? !!keyConfigured[keyDlg.model] : false}
	onClose={() => {
		keyDlg = { open: false, model: '', label: '' };
	}}
	onConfirm={confirmMediaKey}
/>

<style>
	:global(.media-section) {
		max-width: 760px;
	}
	:global(.media-section) .form-row :global(.md-number-field) {
		width: min(100%, var(--md-comp-settings-number-width));
		flex: 0 1 var(--md-comp-settings-number-width);
	}
	.card-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	:global(.settings-card) {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
	}
	.card-head {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.card-title {
		font-size: var(--md-sys-typescale-body-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		color: var(--md-sys-color-primary);
	}
	.card-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: 0;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.form-row {
		display: flex;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
		gap: var(--md-sys-space-md);
	}
	.form-row label {
		width: var(--md-comp-settings-label-width);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		flex-shrink: 0;
	}
	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.capability-block {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-md);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
	}
	.capability-block.first {
		margin-top: 0;
		padding-top: 0;
		border-top: none;
	}
	.capability-block h4 {
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-xs);
	}
	.stt-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: var(--md-sys-space-md);
		align-items: end;
	}
	.model-field {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.model-field .md-input {
		width: 100%;
	}
	.model-field :global(.md-number-field),
	.model-field :global(.md-select-container),
	.model-field :global(.ma-root) {
		width: 100%;
	}
	.field-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
	}
	.md-slider {
		-webkit-appearance: none;
		appearance: none;
		width: 100%;
		height: 4px;
		outline: none;
		cursor: pointer;
		flex: 1;
		margin: 18px 0;
		padding: 0;
		background: transparent;
	}
	.md-slider::-webkit-slider-runnable-track {
		height: 4px;
		border-radius: 2px;
		background: linear-gradient(
			to right,
			var(--md-sys-color-primary) var(--vad-fill, 50%),
			var(--md-sys-color-surface-container-highest) var(--vad-fill, 50%)
		);
	}
	.md-slider::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
		margin-top: -6px;
	}
	.md-slider::-moz-range-track {
		height: 4px;
		border-radius: 2px;
		background: var(--md-sys-color-surface-container-highest);
		border: none;
	}
	.md-slider::-moz-range-thumb {
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		border: none;
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
	}
	.range-value {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 40px;
		min-width: 44px;
		padding: 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	@media (max-width: 700px) {
		.form-row {
			flex-direction: column;
			align-items: stretch;
			gap: var(--md-sys-space-xs);
		}
		:global(.media-section) .form-row :global(.md-number-field) {
			width: min(100%, var(--md-comp-settings-number-width));
			flex: 0 1 auto;
		}
		.form-row label {
			width: auto;
			flex-shrink: 1;
		}
		.switch-row {
			flex-direction: row;
			align-items: center;
			justify-content: space-between;
		}
		.switch-label {
			flex: 1;
			min-width: 0;
		}
	}
</style>
