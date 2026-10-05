# ADR 0486：统一 MCP 与 Skill 直调授权策略来源

## 状态

已采纳并实施（2026-10-05）。

## 背景

MCP/Skill adapter 在 `haven-tools::adapters` 中通过 `OperationPolicy::external` 构造外部执行策略。App 的 `mcp_tool_call` 与 `execute_skill` 为 UI 直调重建同一策略：相同的 qualified capability key、High 风险、`Opaque` 网络边界、ExternalEffect、Session scope、Exclusive concurrency 与 Unknown idempotency，但分别调用 `OperationPolicy::native` 并由名字推导 `data_sensitivity=None`。adapter 策略将同一不可检查的外部数据标为 `Sensitive`。

两者目前仍要求相同的披露确认，因为 `requires_disclosure_confirmation` 对 `NetworkAccess::Opaque` 返回 true；Deny/Restricted 与 WorkspaceWrite 技术边界也由同一 `network_access` 仲裁。本切片统一策略来源和准确的敏感度元数据，不改变用户批准/阻止路径。

## 决定

1. MCP/Skill UI 直调使用 `OperationPolicy::external(qualified_name, RiskLevel::High)`，与对应 `McpToolAdapter` / `SkillToolAdapter` 共用策略构造器。
2. 删除 app command 中重复的 `permission_key`、`NetworkAccess::Opaque` 与风险策略组合。对 `mcp::…`、`skill::…` 名称，现有 `permission_key` 返回裸 qualified name，因此 capability key 与永久/会话 grant 匹配保持不变。
3. direct command 继续使用 `AuthorizationRequest::new(None, …)`，不读取 session-scoped grant；确认队列、MCP/Skill 实际调用位置、风险级别和 opaque network gate 不变。

## 替代方案

- 继续从 app 命令各自拼装 `native` 策略：拒绝。会继续让 App 成为 adapter 的第二个安全策略 owner，并维持 `Sensitive` / `None` 元数据漂移。
- 改成通过 Tools registry 查找 live adapter 并从其提取策略：拒绝。UI 直调可能没有对应的已注册 Tool，缺失时不能退回更宽松的 unknown policy。

## 验证与影响

静态核对 MCP/Skill 适配器和直调统一调用 `OperationPolicy::external`，qualified name 与参数路由 key 行为不变；运行 `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与 `git diff --check`。

直调授权请求的 `data_sensitivity` 由 `None` 改为与 adapter 一致的 `Sensitive`。由于 opaque network 本来就触发披露确认，此元数据校准不放宽或新增实际批准路径；无 schema、数据库、IPC 形状或用户数据迁移。回滚可恢复 app 侧 `native` 构造，但会重新引入策略双源与分类漂移。
