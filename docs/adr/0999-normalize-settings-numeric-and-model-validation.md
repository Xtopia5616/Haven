# ADR 0999：统一 Settings 数值类型与模型配置完整性校验

## 状态

已采纳并实施。

## 背景

ADR 0986/0988 已统一 Settings 数值范围和配置语义校验，但 UI 数值递归只在值已经是 JavaScript number 时执行约束。错误类型（例如字符串 12）会越过 UI 预检，直到 Tauri 解码才失败；生成的 Settings contract 里可空与必填数值也没有由同一 UI 边界验证。另有 validate_settings_contract 只检查模型的 Provider 引用，AppConfig::apply_settings 会静默移除缺少稳定 ID 或未分配 Provider 服务模型的模型；重复配置 ID 则使按 ID 查询模型的结果依赖列表顺序。

## 决定

1. Rust validate_settings_contract 对最终 Settings 模型逐项要求非空配置 id、唯一配置 id、非空 Provider 服务 model 和有效 provider_name。错误包含对应 llm.models[index] 字段路径。
2. ModelConfig::is_assigned 将空白 Provider 名称和服务模型名称视为未分配。AppConfig 仍可保留未绑定模型供路由发现；完整性限制只作用于最终 Settings 写入契约。
3. Settings 页面在凭据 staging 和 IPC 调用前执行同一组模型草稿检查，拒绝空 ID、重复 ID、空服务模型及无效 Provider 绑定。
4. UI 数值边界明确列出生成 Settings/Input contract 中的数值字段。必填数值拒绝 null、字符串和其它错误类型；可空数值按对应字段接受 null；错误信息包含路径。整数宽度、安全整数、浮点范围及 MCP 间隔规则沿用既有校验。
5. 不改变 IPC 字段名、持久化结构、数据库 schema 或模型发现阶段的未绑定状态。

## 影响与验证

- Rust 覆盖空白模型 ID、空白服务模型名和重复模型 ID；Settings 仍只拒绝最终不完整模型。
- UI 覆盖必填/可空数值的错误类型、字段空值边界及模型草稿的 ID、服务模型和 Provider 完整性。
- 从 generatedCommands.ts 的 SettingsInput 可达类型复核 UI 数值字段集合：本次集合无缺项；移除不属于 Settings wire contract 的过期 top_k 数值项。
- 验证：cargo test --workspace --locked、cargo clippy --workspace --locked -- -D warnings、corepack pnpm run check、corepack pnpm run test:run、cargo fmt --all -- --check、IPC contract/ADR 索引检查。

## 回滚

恢复只检查数值范围与 Provider 引用的旧逻辑会重新允许错误类型进入 IPC，并允许 Settings 保存后静默丢弃或产生歧义的模型；无需数据迁移。
