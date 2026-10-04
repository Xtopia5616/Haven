# ADR 0253：RuntimeConfigCoordinator 统一 Router runtime 应用

- 状态：已采纳（2026-09-24）
- 范围：`haven-app-binary` 的 settings/model Router runtime 更新
- 关联：[ADR 0235](0235-versioned-runtime-config-apply-boundary.md)

## 背景

settings 与 model 命令都需要把已提交的 `ConfigSnapshot` 转换为 Router 及其媒体客户端，
再发布到 Agent 和 Tools。原实现由 commands 层分别持有 prepare/publish 编排，model 另有
`rebuild_router` 包装；重复路径容易让版本、失败和发布顺序发生漂移。

## 决定

1. 由 `RuntimeConfigCoordinator` 所有 Router runtime 的 prepare、publish 和 gate。
2. model 使用一次完整的 prepare→publish；settings 仍由 coordinator 分阶段 prepare/publish，
   以保留 security、MCP、context limits 等既有副作用顺序。
3. prepare 只接受已提交的 immutable snapshot；准备失败不发布新 runtime，也不开始 settings
   的后续 live 副作用。
4. 保留 `ConfigApplyGate` 类型别名作为 composition-root 字段的兼容名称，但删除 commands
   层的 `rebuild_router`、`prepare_router_runtime` 与 `publish_router_runtime` 重复入口。

## 影响与验证

Router、STT/OCR/TTS/image client 的构造、版本、错误脱敏和跨 Agent/Tools 发布顺序不变。
测试覆盖 prepare 成功、媒体准备失败不泄漏敏感字段、prepare 失败不调用 apply、成功调用
apply 及 settings/model gate 串行。无 schema、IPC 或配置格式变化。

## 回滚

回退本切片提交即可恢复 commands 层 helper；无需数据库重置。
