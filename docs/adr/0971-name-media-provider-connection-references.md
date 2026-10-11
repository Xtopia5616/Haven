# ADR 0971：区分媒体配置的连接引用与供应商身份

## 状态

已采纳并实施。

## 背景

`media.stt.provider`、`media.tts.provider` 与 `media.image_gen.provider` 保存的是 `llm.providers[].name` 连接名。它们与 `ProviderConfig.provider` 的供应商身份不同；STT、TTS 和图像生成 resolver 还会把连接名解析成运行时派发 backend，并继续将该 backend 保存在 `Resolved*Config.provider`。相邻字段都叫 `provider`，容易把连接选择、供应商身份和运行时 dispatch 混为一谈。`ModelConfig.provider_name` 已为相同的连接引用采用明确字段名（ADR 0632）。

## 决定

1. `SttConfig`、`TtsConfig` 与 `ImageGenConfig` 的字段改为 `provider_name`，并在 TOML、Serde JSON、Tauri DTO 和 generated TypeScript contract 中统一使用该名称。
2. Settings 编辑草稿使用 `providerName`；加载和保存时只在 UI 边界映射到 wire 字段 `provider_name`。
3. `OcrConfig.provider` 保留，表示 OCR 能力选择；`ProviderConfig.provider`、`ModelEndpoint.provider`、`ModelInfo.provider` 与解析后的媒体配置 `provider` 保留，分别表示供应商身份或已选运行时 backend。
4. 旧媒体配置键 `provider` 不增加 alias。严格配置解析拒绝旧字段，按既有流程备份原文件并使用默认配置；用户可以手动将三个媒体表内的 key 改为 `provider_name`，也可以仅重建 `config.toml`。数据库不变。

## 替代方案

- 将所有含 `provider` 的字段批量替换为 `provider_name`：拒绝；供应商身份、OCR 能力和运行时 backend 并非连接名。
- 只改 Rust 内部字段并保留旧 TOML/IPC 名称：拒绝；配置与生成契约会继续呈现歧义。
- 给旧字段增加 Serde alias：拒绝；Haven 自有持久配置不保留隐式旧名兼容入口。

## 影响与验证

- 三种媒体连接引用在 Rust、配置文件、IPC 和 UI 设置草稿中有一致的连接名语义。
- 配置测试覆盖三个新 key、连接名不同于供应商的 STT provider，并逐一确认三个旧 key 会触发备份和默认配置回退。
- UI 设置保存测试覆盖 wire→draft→wire 的三个媒体连接引用，并确认 OCR 的 `provider` 仍保持其能力选择语义。
- 按配置严格契约，变更不需要数据库重置；发布说明提供手动改名与仅重建配置的路径。

## 回滚

恢复三个 Rust 字段、resolver 与 UI 映射为旧名称，并同步恢复配置、生成 IPC 类型、命名文档、发布说明和测试。使用新 `provider_name` key 的配置需在回滚前将三个媒体表中的字段改回旧名；数据库无需调整。
