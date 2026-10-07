# ADR 0728：模型发现请求显式使用 RequestKind

## 状态

已采纳并实施。

## 背景

`discover_models` 的可选 Tauri 参数 `role: String` 仅被检查是否为 `RequestKind::Transcription`，用来切换到 STT 模型发现配置。实际调用方是媒体设置页传 `transcription`；普通 Provider discovery 省略该字段。参数既不是 transcript role，也不接受模型 ID，但注释曾声称两者都可传。自由字符串还使生成 IPC 契约丢失已有的 `RequestKind` 闭合值域。

## 决定

- Rust 参数命名为 `request_kind: Option<RequestKind>`，Tauri wire key 为 `requestKind`。
- UI 继续只在 STT discovery 调用中提供 `requestKind: 'transcription'`；普通 Provider discovery 省略该参数。
- handler 直接匹配 `RequestKind::Transcription`，不再解析 role 字符串。STT credential resolution、Provider lookup 与其它模型发现行为不变。
- 保留 `switch_model`、`set_reasoning_effort`、`set_web_search` 上的 `role` 字段于独立审计项；这些命令的选择器语义不同，不由本决定推断。

## 替代方案

- 将字段命名为 `model_id`：拒绝。discovery 查询 provider 的模型列表，没有被选中的 `ModelConfig`。
- 保留 `role` 并只补注释：拒绝。字段名仍指向消息角色，类型仍允许无效值。
- 用 boolean `is_transcription`：拒绝。项目已有 `RequestKind` 闭合类型，重用它能明确表达领域选择器。

## 影响与验证

Rust command signature 从 `role: Option<String>` 改为 `request_kind: Option<RequestKind>`；generated Tauri request 从 `role?: string` 改为 `requestKind?: RequestKindInput`，后者是 Rust `RequestKind` 的输入 union。只有 UI 的 STT discovery 请求携带新键；无数据库、配置持久化或 provider wire 变化，无需重置数据。

验证：Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC 生成与漂移检查、事件检查、ADR 索引及 diff checks。

## 回滚

恢复 handler 的 `role: Option<String>`、旧生成契约与 STT caller 字段即可；不涉及持久数据。
