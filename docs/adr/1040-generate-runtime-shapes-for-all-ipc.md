# 1040：为全部 IPC 命令与事件生成运行时 wire shape

## 状态

已采纳并实施（2026-10-12）。

## 背景

前端部分 command wrapper 和 event mapper 已有手写字段校验，但覆盖集合与 Rust handler/Serde DTO 不一致。字段新增、缺失或类型漂移可能只在少数调用路径被发现；事件渠道也缺少一项检查，确保每个 channel 都有结构校验。

## 决定

- Rust handler 成功返回类型及其 Serde DTO 是 command response wire shape 的唯一来源。IPC 生成器为每个已登记的 Tauri command 输出运行时 shape registry。
- 事件生成器显式登记全部 36 个 Rust event channel 与 payload DTO 的对应关系；新增、删除或改名 channel 时，缺少映射必须使生成失败。
- 前端统一在 command `invoke` 边界验证 response shape；每个事件 domain mapper 在语义投影前验证对应 channel 的 shape。
- 结构校验允许 DTO 新增未知字段，保留向前兼容；闭合 enum、规范 ID、跨字段约束、状态转移和安全规则继续由现有 domain mapper/owner 校验。
- `serde_json::Value` 等动态扩展点仍保持动态；生成器不为其臆造固定字段。
- Rust response-only 自定义反序列化字段仍按可序列化 wire 类型生成；request shape 对自定义反序列化 fail closed，因为其接受集合不能从 Serde 元数据可靠推导。

## 影响

命令响应与事件负载的必填字段、可选字段和基本值形状由 Rust 类型自动覆盖，避免运行时校验表与 handler 登记表各自增长。生成 TS 文件会包含 shape registry；事件 mapper 与中心 command wrapper 在校验失败时拒绝该 payload/response，并且日志不输出其内容。IPC wire shape 与持久格式没有改变，无需数据库、配置或数据重置。

## 替代方案

继续逐个手写 parser 或仅靠 TypeScript 静态类型不能验证运行时 IPC 数据，也不能保证所有 channel/command 都被覆盖，因此不采用。

## 验证

- `cargo test --locked -p haven-app-binary --features ipc-codegen --bin generate_ipc_contracts`
- `pwsh -NoProfile -File scripts/generate-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-ipc-events.ps1`
- `corepack pnpm run check` 与 `corepack pnpm run test:run`

## 回滚

若校验导致合法 wire payload 被拒绝，先修正 DTO 生成器或对应 Rust Serde 契约和 mapper 测试。若整体移除运行时检查，必须同时恢复全量 command/event 覆盖门禁；不能只删 generated registry 而保留不完整的手写局部校验。
