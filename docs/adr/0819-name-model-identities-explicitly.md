# ADR 0819：明确区分模型配置 ID 与 Provider 服务模型 ID

## 状态

Accepted — 2026-10-08

## 背景

Haven 的 `ModelConfig.id` 是本地稳定配置身份，`ModelConfig.model` 是发给 Provider 的模型标识；一个服务模型可以被多个配置以不同参数引用。TOML 的历史字段名和 `switch_model.modelId` 容易让调用方把二者混用。模型健康诊断只给 `not_configured`，未说明缺少路由策略、引用失效、能力声明或凭据。

## 决定

1. 两个 ID 不合并：路由策略和 Haven 内部选择使用模型配置 ID；Provider API 的 `model` 字段使用服务模型 ID。
2. `switch_model` 请求字段重命名为 `model_config_id`（wire: `modelConfigId`）；保留 `config.toml` 现有 `id`、`model`、`primary` 字段，避免本版本丢弃用户配置。
3. 设置页分开说明两种 ID 的边界；Rust 配置类型文档和架构文档明确对应关系。
4. diagnostics 将两个 ID 作为独立字段显示，并用稳定原因码区分缺失策略、缺失模型配置、不完整绑定、缺少能力声明和凭据缺失。健康检查仍只代表端点探测成功，不宣称 Provider 模型目录中一定存在该服务 ID。

## 替代方案

- 将模型配置 ID 和服务模型 ID合并：拒绝。配置级温度、上下文窗口、成本、能力和 reasoning overrides 需要独立的本地配置身份。
- 直接重命名 TOML 字段：拒绝。本次优先修复 ID 语义混淆；改持久化 schema 会要求配置迁移或重建，不是修复 IPC 命名所必需。

## 影响与验证

`config.toml` shape 与现有模型配置无需重置；`switch_model` 内部 IPC 字段从 `modelId` 变为 `modelConfigId`，由 Rust contract generator 更新 UI 类型。诊断输出只增加非敏感路由信息。回归覆盖 UI payload、路由身份不变、两个 ID 同时呈现以及每种未配置原因。

适用门禁：IPC contract generation/check、Rust workspace check/test/strict Clippy、UI check/test/build。

## 回滚

回滚 IPC 字段和诊断展示即可；TOML 无需迁移或重置。
