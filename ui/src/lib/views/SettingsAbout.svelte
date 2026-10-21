<script lang="ts">
	import { onMount } from 'svelte';
	import { getVersion } from '@tauri-apps/api/app';
	import { resolveResource } from '@tauri-apps/api/path';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import SettingsSection from '$lib/SettingsSection.svelte';
	import { isTauri } from '$lib/tauri.ts';
	import { openExternal } from '$lib/externalRef.ts';

	let appVersion = $state('');
	const repositoryUrl = 'https://github.com/Xtopia5616/Haven';

	async function openBundledDocument(resourcePath: string, onlineUrl: string) {
		if (!isTauri()) {
			void openExternal(onlineUrl);
			return;
		}

		try {
			const resolvedPath = await resolveResource(resourcePath);
			await openExternal(resolvedPath);
		} catch {
			void openExternal(onlineUrl);
		}
	}

	onMount(() => {
		if (!isTauri()) {
			appVersion = '浏览器预览';
			return;
		}

		void getVersion()
			.then((version) => (appVersion = version))
			.catch(() => (appVersion = '未知'));
	});
</script>

<div class="settings-about">
	<SettingsSection title="Haven" description="面向 Windows 的本机语音助手。">
		<p class="about-version">版本：{appVersion || '读取中…'}</p>
		<div class="about-actions">
			<MaterialButton
				label="GitHub 项目"
				variant="outlined"
				onclick={() => void openExternal(repositoryUrl)}
			/>
			<MaterialButton
				label="作者主页"
				variant="outlined"
				onclick={() => void openExternal('https://chensiyuan.work')}
			/>
		</div>
		<p class="about-author">作者：Chen Siyuan</p>
	</SettingsSection>

	<SettingsSection title="许可证">
		<p>
			Haven 自有代码以 MIT License 发布。字体、图标、语音检测模型和软件依赖保留各自的原始许可证；这些第三方许可不因 Haven 使用 MIT 而改变。
		</p>
		<div class="about-actions">
			<MaterialButton
				label="项目 MIT License"
				variant="text"
				onclick={() =>
					void openBundledDocument(
						'licenses/LICENSE',
						`${repositoryUrl}/blob/main/LICENSE`,
					)}
			/>
			<MaterialButton
				label="第三方许可证与致谢"
				variant="text"
				onclick={() =>
					void openBundledDocument(
						'licenses/THIRD_PARTY_LICENSES.md',
						`${repositoryUrl}/blob/main/THIRD_PARTY_LICENSES.md`,
					)}
			/>
			<MaterialButton
				label="完整依赖许可证清单"
				variant="text"
				onclick={() =>
					void openBundledDocument(
						'licenses/dependency-license-inventory.md',
						`${repositoryUrl}/blob/main/docs/dependency-license-inventory.md`,
					)}
			/>
			<MaterialButton
				label="Silero VAD 模型许可证"
				variant="text"
				onclick={() =>
					void openBundledDocument(
						'licenses/LICENSE-Silero-VAD.txt',
						'https://github.com/snakers4/silero-vad/blob/master/LICENSE',
					)}
			/>
			<MaterialButton
				label="字体 OFL 许可证"
				variant="text"
				onclick={() =>
					void openBundledDocument(
						'ui/fonts/OFL.txt',
						'https://openfontlicense.org',
					)}
			/>
		</div>
	</SettingsSection>

	<SettingsSection title="开源组件与致谢">
		<ul class="about-credits">
			<li>
				<strong>Silero Team — Silero VAD v5</strong>：随应用打包的语音活动检测模型，MIT License。
			</li>
			<li>
				<strong>Roboto</strong>：界面字体，Copyright 2011 The Roboto Project Authors，SIL Open Font License 1.1。
			</li>
			<li>
				<strong>JetBrains Mono</strong>：等宽字体，Copyright 2020 The JetBrains Mono Project Authors，SIL Open Font License 1.1。
			</li>
			<li>
				<strong>Lucide Icons 与 Feather</strong>：图标路径来源，分别适用 ISC License 和 MIT License。
			</li>
			<li>
				<strong>Pi Coding Agent</strong>：ReAct agent 架构参考；Haven 的实现和集成代码由本项目维护。
			</li>
			<li>
				<strong>Google Material Design 3 Expressive</strong>：界面设计规范参考；Haven 的界面组件由本项目实现。
			</li>
			<li>
				<strong>Agent Skills 与 Model Context Protocol</strong>：Haven 分别按公开清单格式和协议实现互操作。
			</li>
		</ul>
		<p>
			模型 Provider 适配器按各服务公开的 API 文档实现；交互示例和测试 fixture 由 Haven 维护。固定版本的 Rust 与 Node 依赖及其许可证见完整清单。
		</p>
	</SettingsSection>
</div>

<style>
	.settings-about {
		min-width: 0;
	}

	.about-version,
	.about-author {
		margin: 0 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
	}

	.about-actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}

	.about-credits {
		margin: 0 0 var(--md-sys-space-md);
		padding-left: var(--md-sys-space-xl);
		color: var(--md-sys-color-on-surface);
	}

	.about-credits li + li {
		margin-top: var(--md-sys-space-sm);
	}

	.settings-about p {
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
</style>
