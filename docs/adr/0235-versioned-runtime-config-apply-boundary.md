# ADR 0235：版本化运行时配置应用边界

- 状态：已采纳（2026-09-24）
- 范围：`haven-app-binary` 的 settings/model 配置更新与 Router/媒体运行时应用
- 关联：[ADR 0216](0216-runtime-config-coordinator.md)、[ADR 0230](0230-context-limits-router-refresh-order.md)

## 背景

`ConfigService` 已经以版本化 snapshot 持久化配置，但 settings 与 model 命令仍可能在保存后重新读取当前配置来构建
Router 和媒体客户端。这样会让一次应用混用不同版本，并且媒体策略可能在后续可失败的客户端构建前先更新。
两个入口也没有共享应用串行边界。

## 决定

1. `ApplicationRuntime::config_apply_gate` 串行化 settings 与 model 的提交加应用过程，避免旧 snapshot 在新提交之后发布。
2. Router、STT、OCR、TTS、图像生成客户端和 media config 必须从同一个已提交 `ConfigSnapshot` 预构建；应用路径不得为此重新读取 `ConfigService`。
3. 所有这些可失败的准备完成后，才发布 Agent/Tools Router 与媒体运行时，并继续按既有顺序应用 ContextLimits 等消费者。
4. 准备失败时持久化配置保留新版本，live runtime 保持旧代；不引入补偿回滚。
5. 这只是 settings/model 的窄边界，不宣称 Agent 与 Tools 的跨容器原子发布，也不自动覆盖 admin/MCP/Skills/logging 的其他写入入口。

## 替代方案

- 每个命令保存后重新读取配置：会产生跨版本组合，拒绝。
- 在可失败准备前更新媒体或 limits：失败时会留下混代 live runtime，拒绝。
- 立即把所有配置写入入口纳入全局事务：写集和失败语义过大，延期到后续协调器切片。

## 影响与验证

配置格式、数据库、IPC、provider wire 和持久化先行语义不变，无需重置。新增 coordinator helper、应用锁、同 snapshot
准备和失败/并发测试；敏感配置只进入 sanitized error，不进入测试日志。

验证：`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary`、
`cargo clippy --locked -p haven-app-binary -- -D warnings`。

## 回滚

删除 `ConfigApplyGate` 和 prepared runtime 路径，恢复 settings/model 原有重建入口；不涉及 schema、配置文件或用户数据。
