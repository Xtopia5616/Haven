<script>
	import { onMount } from 'svelte';
	import { invoke } from '$lib/tauri.js';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import { addNotification } from '$lib/stores.js';

	let llmConfig = $state({
		small_model: { provider: 'openai', model_name: 'gpt-4o-mini', temperature: 0, base_url: '', api_key: '' },
		default_model: { provider: 'anthropic', model_name: 'claude-sonnet-4-20250514', temperature: 0.7, base_url: '', api_key: '' },
		balanced_model: { provider: 'local', model_name: 'llama3', temperature: 0.7, base_url: 'http://localhost:11434', api_key: '' },
	});

	let keyConfigured = $state({
		small_model: false,
		default_model: false,
		balanced_model: false,
	});

	let hotkeyMode = $state('toggle');
	let hotkeyBinding = $state('Ctrl+Shift+Space');
	let autostartEnabled = $state(false);

	let audio = $state({
		sample_rate: 16000,
		channels: 1,
		bits_per_sample: 16,
		max_duration_secs: 60,
		silence_timeout_ms: 1500,
		vad_threshold: 0.5,
	});

	let task = $state({ max_concurrent: 3, max_steps: 30 });
	let memory = $state({ session_window_size: 50, history_retention_days: 90 });
	let security = $state({ confirmation_mode: 'always', min_risk_level: 'low' });

	let stt = $state({ provider: 'mcp', mcp_server: '', timeout_secs: 30 });
	let notification = $state({
		task_created: { in_app: true, windows: false },
		task_completed: { in_app: true, windows: true },
		task_paused: { in_app: true, windows: false },
		task_error: { in_app: true, windows: true },
		task_cancelled: { in_app: true, windows: false },
	});
	let log = $state({ level: 'info', file_enabled: true, max_file_size_mb: 10, max_files: 5 });

	let preferences = $state([]);
	let prefLoaded = $state(false);

	let keyChangeDialog = $state({ open: false, model: '', label: '' });
	let newKeyValue = $state('');

	onMount(async () => {
		try {
			const settings = await invoke('get_settings');
			if (settings) {
				llmConfig = settings.llm || llmConfig;
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				audio = settings.audio || audio;
				task = settings.task || task;
				memory = settings.memory || memory;
				security = {
					confirmation_mode: settings.security?.confirmation_mode || 'always',
					min_risk_level: settings.security?.min_risk_level || 'low',
				};
				stt = {
					provider: settings.stt?.provider || 'mcp',
					mcp_server: settings.stt?.mcp_server || '',
					timeout_secs: settings.stt?.timeout_secs || 30,
				};
				notification = settings.notification || notification;
				log = settings.log || log;
			}
		} catch (e) {
			console.warn('load settings error:', e);
		}
		try {
			keyConfigured = await invoke('get_api_key_status');
		} catch (e) {
			console.warn('get_api_key_status error:', e);
		}
		try {
			autostartEnabled = await invoke('is_autostart_enabled');
		} catch (e) {
			console.warn('is_autostart_enabled error:', e);
		}
		await loadPreferences();
	});

	async function loadPreferences() {
		try {
			const all = await invoke('list_preferences');
			// filter out tool_usage counters (internal)
			preferences = (all || []).filter(([k]) => !k.startsWith('tool_usage.') && !k.startsWith('tool_param.') && !k.startsWith('cfg.'));
			prefLoaded = true;
		} catch {
			preferences = [];
		}
	}

	async function deletePrefKey(key) {
		try {
			await invoke('delete_preference', { key });
			preferences = preferences.filter(([k]) => k !== key);
		} catch (e) {
			console.warn('delete preference error:', e);
		}
	}

	async function saveSettings() {
		try {
			await invoke('update_settings', {
				settings: {
					llm: llmConfig,
					hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: null },
					audio: {
						sample_rate: audio.sample_rate,
						channels: audio.channels,
						bits_per_sample: audio.bits_per_sample,
						max_duration_secs: audio.max_duration_secs,
						silence_timeout_ms: audio.silence_timeout_ms,
						vad_threshold: audio.vad_threshold,
					},
					task: {
						max_concurrent: task.max_concurrent,
						max_steps: task.max_steps,
						default_priority: 'normal',
					},
					memory: {
						session_window_size: memory.session_window_size,
						history_retention_days: memory.history_retention_days,
					},
				security: {
					confirmation_mode: security.confirmation_mode,
					min_risk_level: security.min_risk_level,
					encrypt_sensitive: true,
				},
					stt: {
						provider: stt.provider,
						mcp_server: stt.mcp_server || null,
						timeout_secs: stt.timeout_secs,
					},
					notification: {
						task_created: { in_app: notification.task_created.in_app, windows: notification.task_created.windows },
						task_completed: { in_app: notification.task_completed.in_app, windows: notification.task_completed.windows },
						task_paused: { in_app: notification.task_paused.in_app, windows: notification.task_paused.windows },
						task_error: { in_app: notification.task_error.in_app, windows: notification.task_error.windows },
						task_cancelled: { in_app: notification.task_cancelled.in_app, windows: notification.task_cancelled.windows },
					},
					log: {
						level: log.level,
						file_enabled: log.file_enabled,
						max_file_size_mb: log.max_file_size_mb,
						max_files: log.max_files,
						file_path: null,
					},
				},
			});
		addNotification('Settings saved', 'success');
			if (autostartEnabled) {
				try { await invoke('enable_autostart'); } catch (e) {
					autostartEnabled = false;
					addNotification(`自动启动：${e}`, 'warning');
				}
			} else {
				try { await invoke('disable_autostart'); } catch (e) {
					autostartEnabled = true;
					addNotification(`取消自动启动：${e}`, 'warning');
				}
			}
		} catch (e) {
			console.error('save failed', e);
		}
	}

	async function toggleAutostart() {
		autostartEnabled = !autostartEnabled;
	}

	function openKeyDialog(model, label) {
		keyChangeDialog = { open: true, model, label };
		newKeyValue = '';
	}

	function confirmKeyChange() {
		if (newKeyValue.trim()) {
			llmConfig[keyChangeDialog.model].api_key = newKeyValue.trim();
			keyConfigured[keyChangeDialog.model] = true;
		}
		keyChangeDialog = { open: false, model: '', label: '' };
		newKeyValue = '';
	}
</script>

<div class="settings-page">
	<h1>Settings</h1>

	<div class="section">
		<h2>LLM Configuration</h2>

		<div class="model-card">
			<h3>Small Model</h3>
			<p class="model-hint">Fast classification &amp; lightweight reasoning</p>
			<div class="form-row">
				<label for="sm-provider">Provider</label>
				<input id="sm-provider" type="text" class="md-input" bind:value={llmConfig.small_model.provider} />
			</div>
			<div class="form-row">
				<label for="sm-model">Model</label>
				<input id="sm-model" type="text" class="md-input" bind:value={llmConfig.small_model.model_name} />
			</div>
			<div class="form-row">
				<label for="sm-base-url">Base URL</label>
				<input id="sm-base-url" type="text" class="md-input" bind:value={llmConfig.small_model.base_url} placeholder="https://api.openai.com/v1" />
			</div>
			<div class="form-row">
				<label for="sm-api-key">API Key</label>
				{#if keyConfigured.small_model}
					<div class="key-status-row">
						<StatusDot />
						<span class="key-configured-label">Configured</span>
						<button
							class="md-btn md-btn--xs md-btn--outlined"
							onclick={() => openKeyDialog('small_model', 'Small Model')}
						>
							Change
						</button>
					</div>
				{:else}
					<input id="sm-api-key" type="password" class="md-input" bind:value={llmConfig.small_model.api_key} placeholder="sk-..." />
				{/if}
			</div>
			<div class="form-row">
				<label for="sm-temp">Temperature</label>
				<MaterialNumberField id="sm-temp" value={llmConfig.small_model.temperature} step={0.1} min={0} max={2} onChange={(v) => { llmConfig.small_model.temperature = v; }} />
			</div>
		</div>

		<div class="model-card">
			<h3>Default Model</h3>
			<p class="model-hint">Primary reasoning &amp; tool-use agent</p>
			<div class="form-row">
				<label for="dm-provider">Provider</label>
				<input id="dm-provider" type="text" class="md-input" bind:value={llmConfig.default_model.provider} />
			</div>
			<div class="form-row">
				<label for="dm-model">Model</label>
				<input id="dm-model" type="text" class="md-input" bind:value={llmConfig.default_model.model_name} />
			</div>
			<div class="form-row">
				<label for="dm-base-url">Base URL</label>
				<input id="dm-base-url" type="text" class="md-input" bind:value={llmConfig.default_model.base_url} placeholder="https://api.openai.com/v1" />
			</div>
			<div class="form-row">
				<label for="dm-api-key">API Key</label>
				{#if keyConfigured.default_model}
					<div class="key-status-row">
						<StatusDot />
						<span class="key-configured-label">Configured</span>
						<button
							class="md-btn md-btn--xs md-btn--outlined"
							onclick={() => openKeyDialog('default_model', 'Default Model')}
						>
							Change
						</button>
					</div>
				{:else}
					<input id="dm-api-key" type="password" class="md-input" bind:value={llmConfig.default_model.api_key} placeholder="sk-..." />
				{/if}
			</div>
			<div class="form-row">
				<label for="dm-temp">Temperature</label>
				<MaterialNumberField id="dm-temp" value={llmConfig.default_model.temperature} step={0.1} min={0} max={2} onChange={(v) => { llmConfig.default_model.temperature = v; }} />
			</div>
		</div>

		<div class="model-card">
			<h3>Balanced Model</h3>
			<p class="model-hint">Fallback when primary model is unavailable</p>
			<div class="form-row">
				<label for="bm-provider">Provider</label>
				<input id="bm-provider" type="text" class="md-input" bind:value={llmConfig.balanced_model.provider} />
			</div>
			<div class="form-row">
				<label for="bm-model">Model</label>
				<input id="bm-model" type="text" class="md-input" bind:value={llmConfig.balanced_model.model_name} />
			</div>
			<div class="form-row">
				<label for="bm-base-url">Base URL</label>
				<input id="bm-base-url" type="text" class="md-input" bind:value={llmConfig.balanced_model.base_url} placeholder="http://localhost:11434" />
			</div>
			<div class="form-row">
				<label for="bm-api-key">API Key</label>
				{#if keyConfigured.balanced_model}
					<div class="key-status-row">
						<StatusDot />
						<span class="key-configured-label">Configured</span>
						<button
							class="md-btn md-btn--xs md-btn--outlined"
							onclick={() => openKeyDialog('balanced_model', 'Balanced Model')}
						>
							Change
						</button>
					</div>
				{:else}
					<input id="bm-api-key" type="password" class="md-input" bind:value={llmConfig.balanced_model.api_key} placeholder="sk-..." />
				{/if}
			</div>
			<div class="form-row">
				<label for="bm-temp">Temperature</label>
				<MaterialNumberField id="bm-temp" value={llmConfig.balanced_model.temperature} step={0.1} min={0} max={2} onChange={(v) => { llmConfig.balanced_model.temperature = v; }} />
			</div>
		</div>
	</div>

	<div class="section">
		<h2>Hotkeys</h2>
		<div class="form-row">
			<label for="hotkey-binding">Key Binding</label>
			<input id="hotkey-binding" type="text" class="md-input" bind:value={hotkeyBinding} />
		</div>
		<div class="form-row">
			<label for="hotkey-mode">Mode</label>
			<MaterialSelect id="hotkey-mode" value={hotkeyMode} options={[{ value: 'toggle', label: 'Toggle (press to start/stop)' }, { value: 'hold', label: 'Hold (push-to-talk)' }]} onChange={(v) => { hotkeyMode = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Audio</h2>
		<div class="form-row">
			<label for="audio-sample-rate">Sample Rate</label>
			<MaterialNumberField id="audio-sample-rate" value={audio.sample_rate} onChange={(v) => { audio.sample_rate = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-channels">Channels</label>
			<MaterialNumberField id="audio-channels" value={audio.channels} min={1} max={2} onChange={(v) => { audio.channels = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-max-duration">Max Duration (sec)</label>
			<MaterialNumberField id="audio-max-duration" value={audio.max_duration_secs} min={10} max={300} onChange={(v) => { audio.max_duration_secs = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-silence-timeout">Silence Timeout (ms)</label>
			<MaterialNumberField id="audio-silence-timeout" value={audio.silence_timeout_ms} min={500} max={10000} step={100} onChange={(v) => { audio.silence_timeout_ms = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-vad-threshold">VAD Threshold</label>
			<input id="audio-vad-threshold" type="range" class="md-slider" bind:value={audio.vad_threshold} min="0" max="1" step="0.05" style="--vad-fill: {audio.vad_threshold * 100}%" />
			<span class="range-value">{audio.vad_threshold}</span>
		</div>
	</div>

	<div class="section">
		<h2>STT (Speech-to-Text)</h2>
		<div class="form-row">
			<label for="stt-provider">Provider</label>
			<MaterialSelect id="stt-provider" value={stt.provider} options={[{ value: 'mcp', label: 'MCP Server' }, { value: 'llm', label: 'LLM Adapter' }, { value: 'none', label: 'None' }]} onChange={(v) => { stt.provider = v; }} />
		</div>
		<div class="form-row">
			<label for="stt-mcp-server">MCP Server Name</label>
			<input id="stt-mcp-server" type="text" class="md-input" bind:value={stt.mcp_server} placeholder="e.g. stt-server" />
		</div>
		<div class="form-row">
			<label for="stt-timeout">Timeout (sec)</label>
			<MaterialNumberField id="stt-timeout" value={stt.timeout_secs} min={5} max={120} onChange={(v) => { stt.timeout_secs = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Task</h2>
		<div class="form-row">
			<label for="task-max-concurrent">Max Concurrent</label>
			<MaterialNumberField id="task-max-concurrent" value={task.max_concurrent} min={1} max={10} onChange={(v) => { task.max_concurrent = v; }} />
		</div>
		<div class="form-row">
			<label for="task-max-steps">Max Steps</label>
			<MaterialNumberField id="task-max-steps" value={task.max_steps} min={1} max={100} onChange={(v) => { task.max_steps = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Memory</h2>
		<div class="form-row">
			<label for="memory-window-size">Window Size</label>
			<MaterialNumberField id="memory-window-size" value={memory.session_window_size} min={10} max={500} onChange={(v) => { memory.session_window_size = v; }} />
		</div>
		<div class="form-row">
			<label for="memory-retention">Retention (days)</label>
			<MaterialNumberField id="memory-retention" value={memory.history_retention_days} min={1} max={365} onChange={(v) => { memory.history_retention_days = v; }} />
		</div>
	</div>

	<div class="section pref-section">
		<h2>Preferences</h2>
		<p class="model-hint" style="margin-bottom: var(--md-sys-space-md)">Learned preferences from your interactions. User-set values take priority over inferred values.</p>
		{#if prefLoaded && preferences.length > 0}
			<div class="pref-list">
				{#each preferences as [key, value]}
					<div class="pref-row">
						<span class="pref-key">{key.trimStart('inferred.')}</span>
						<span class="pref-value">
							{#if value.startsWith('[inferred]')}
								<span class="pref-tag pref-tag--inf">inferred</span>
							{:else if value.startsWith('[user]')}
								<span class="pref-tag pref-tag--user">user</span>
							{/if}
							{value.replace('[inferred] ', '').replace('[user] ', '')}
						</span>
						<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deletePrefKey(key)} title="Delete preference">
							&times;
						</button>
					</div>
				{/each}
			</div>
		{:else if prefLoaded}
			<p class="model-hint">No preferences recorded yet. They will appear here as you use Haven.</p>
		{/if}
	</div>

	<div class="section">
		<h2>Security</h2>
		<div class="form-row">
			<label for="security-min-level">Minimum Confirmation Level</label>
			<MaterialSelect id="security-min-level" value={security.min_risk_level} options={[
				{ value: 'safe', label: 'None (all auto-approved)' },
				{ value: 'low', label: 'Low & above' },
				{ value: 'medium', label: 'Medium & above' },
				{ value: 'high', label: 'High & above' },
				{ value: 'critical', label: 'Critical only' },
			]} onChange={(v) => { security.min_risk_level = v; }} />
		</div>
		<p class="model-hint">Operations at or above this risk level will require your confirmation. Low-level operations (file read, window list) will auto-approve.</p>
	</div>

	<div class="section notification-section">
		<h2>Notifications</h2>
		<div class="notify-grid-header">
			<span class="switch-label"></span>
			<span class="switch-label">In-App Toast</span>
			<span class="switch-label">Windows</span>
		</div>
		{#each [
			{ key: 'task_created', label: 'Task Start' },
			{ key: 'task_completed', label: 'Task Complete' },
			{ key: 'task_paused', label: 'Task Paused' },
			{ key: 'task_error', label: 'Task Error' },
			{ key: 'task_cancelled', label: 'Task Cancelled' },
		] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch checked={notification[ev.key].in_app} onChange={(v) => { notification[ev.key].in_app = v; }} />
				<MaterialSwitch checked={notification[ev.key].windows} onChange={(v) => { notification[ev.key].windows = v; }} />
			</div>
		{/each}
	</div>

	<div class="section log-section">
		<h2>Logging</h2>
		<div class="form-row switch-row">
			<span class="switch-label">File Logging</span>
			<MaterialSwitch checked={log.file_enabled} onChange={(v) => { log.file_enabled = v; }} />
		</div>
		<div class="form-row">
			<label for="log-level">Log Level</label>
			<MaterialSelect id="log-level" value={log.level} options={[
				{ value: 'trace', label: 'Trace' },
				{ value: 'debug', label: 'Debug' },
				{ value: 'info', label: 'Info' },
				{ value: 'warn', label: 'Warn' },
				{ value: 'error', label: 'Error' },
			]} onChange={(v) => { log.level = v; }} />
		</div>
		<div class="form-row">
			<label for="log-max-size">Max File Size (MB)</label>
			<MaterialNumberField id="log-max-size" value={log.max_file_size_mb} min={1} max={100} onChange={(v) => { log.max_file_size_mb = v; }} />
		</div>
		<div class="form-row">
			<label for="log-max-files">Max Files</label>
			<MaterialNumberField id="log-max-files" value={log.max_files} min={1} max={50} onChange={(v) => { log.max_files = v; }} />
		</div>
	</div>

	<div class="section autostart-section">
		<h2>Autostart</h2>
		<div class="form-row autostart-row">
			<span class="autostart-label">Launch Haven on system startup</span>
				<MaterialSwitch checked={autostartEnabled} onChange={toggleAutostart} />
		</div>
	</div>

	<button class="md-btn md-btn--filled save-btn" onclick={saveSettings}>
		Save Settings
	</button>
</div>

<MaterialDialog
	open={keyChangeDialog.open}
	onClose={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; }}
	title="Change API Key"
>
	{#snippet children()}
		<p class="dialog-hint">Enter a new API key for <strong>{keyChangeDialog.label}</strong>. The existing key will be replaced.</p>
		<input
			type="password"
			class="md-input"
			bind:value={newKeyValue}
			placeholder="sk-..."
		/>
	{/snippet}
	{#snippet footer()}
		<button
			class="md-btn md-btn--text"
			onclick={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; }}
		>
			Cancel
		</button>
		<button class="md-btn md-btn--filled" onclick={confirmKeyChange}>Confirm</button>
	{/snippet}
</MaterialDialog>

<style>
	.settings-page { max-width: 800px; }
	h1 { font-size: 24px; font-weight: 600; margin-bottom: var(--md-sys-space-xl); color: var(--md-sys-color-on-surface); }
	.section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-lg);
	}
	.section h2 {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase; letter-spacing: 1px; margin-bottom: var(--md-sys-space-lg);
	}
	.model-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium); padding: var(--md-sys-space-md); margin-bottom: var(--md-sys-space-md);
	}
	.model-card h3 { font-size: 14px; font-weight: 600; color: var(--md-sys-color-primary); margin-bottom: var(--md-sys-space-md); }
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); }
	.form-row {
		display: flex; align-items: center; margin-bottom: var(--md-sys-space-sm); gap: var(--md-sys-space-md);
	}
	.form-row label { width: 120px; color: var(--md-sys-color-on-surface-variant); font-size: 13px; flex-shrink: 0; }

	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}

	.notify-grid-header, .notify-grid-row {
		display: grid;
		grid-template-columns: 1fr auto auto;
		gap: var(--md-sys-space-md);
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.notify-grid-header {
		padding-bottom: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		margin-bottom: var(--md-sys-space-md);
	}
	.notify-grid-header .switch-label {
		font-weight: 600;
		font-size: 11px;
		text-transform: uppercase;
	}

	.form-row input[type='range'] { flex: 1; }
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
		background: linear-gradient(to right, var(--md-sys-color-primary) var(--vad-fill, 50%), var(--md-sys-color-surface-container-highest) var(--vad-fill, 50%));
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
		transition: transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-slider::-webkit-slider-thumb:hover {
		transform: scale(1.25);
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
	.md-slider::-moz-range-thumb:hover {
		transform: scale(1.25);
	}
	.md-slider:focus-visible::-webkit-slider-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.md-slider:focus-visible::-moz-range-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.range-value {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 40px;
		min-width: 44px;
		padding: 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		font-weight: 500;
	}
	.autostart-section .autostart-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.autostart-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.save-btn {
		display: block;
		margin: 0 auto;
	}
	.key-status-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex: 1;
	}
	.key-configured-label {
		color: var(--md-sys-color-success);
		font-size: 13px;
		font-weight: 500;
		flex: 1;
	}
	.dialog-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
		margin-bottom: var(--md-sys-space-lg);
	}
	.pref-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.pref-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
	}
	.pref-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		font-weight: 500;
		min-width: 140px;
		flex-shrink: 0;
	}
	.pref-value {
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.pref-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 20px;
		padding: 0 6px;
		border-radius: var(--md-sys-shape-small);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		flex-shrink: 0;
	}
	.pref-tag--user {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.pref-tag--inf {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.pref-section .model-hint {
		font-size: 12px;
	}
</style>
