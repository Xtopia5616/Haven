# ADR 1055：统一媒体 timeout 范围与 Settings round-trip

## 状态

已接受（2026-10-12）

## 背景

STT、OCR、TTS 与 image generation 的配置字段由 Rust 配置模型和 UI Settings 共用，但有效范围只体现在 UI 的 `MaterialNumberField` 上。Common 配置校验和 UI Settings parser 仅要求 timeout 是安全的非负整数，因此 TOML 与响应都可以包含 `0` 或超出控件范围的值。`SettingsView` 使用 `value || default` 将 `0` 静默替换为默认值，保存时会覆盖原值。运行时将 `0` 传给 provider timeout，会导致请求立即超时；它不表示禁用超时。

## 决定

- 把现有 Settings 控件范围作为 TOML、Settings response 与 `update_settings` request 的共同语义约束：STT 为 5–600 秒，OCR/TTS 为 5–300 秒，image generation 为 10–600 秒。
- Common 配置校验同时用于 TOML load/save 与 Settings update；越界 TOML 按既有语义校验失败策略备份整份配置并在当前进程使用默认值，Settings update 拒绝写入。
- UI 的 Settings response parser 与 update command validator 执行相同范围校验；媒体数值控件从同一个 UI bounds 常量读取最小值和最大值。
- `SettingsView` 对缺省 timeout 使用 nullish fallback，显式数值不因 falsy 判断被悄悄改写。
- 当前架构文档明确区分生成的 request `SettingsUpdatePayload`（Rust `SettingsInput`）与 response `SettingsPayload`。

## 考虑过的替代方案

- 只把 `||` 改成 `??` 会保留 `0`，但仍允许 Rust、TOML 和 UI parser 接受控件不允许编辑的值；provider 对 `0` 的行为也只是立即超时。
- 在 UI 加载时把所有越界值静默 clamp 到控件范围，会改写用户设置而不提供明确配置错误或备份提示。

## 影响与兼容

现有 `config.toml` 中越界的四类媒体 timeout 会按既有配置语义验证策略备份，当前进程使用默认配置；如需保留其余设置，用户先将对应值改到范围内。Settings update 对越界值返回错误且不写入配置。IPC JSON shape 与 SQLite schema 不变，无需重置数据库。

## 验证

- Rust 边界测试覆盖四类 timeout 的最小值、最大值及越界值，并验证 TOML load 备份行为。
- UI contract 测试覆盖 Settings response 与 update validator 的相同边界。
- 完整 Rust、UI 与 IPC contract 适用门禁记录在提交验证结果中。
