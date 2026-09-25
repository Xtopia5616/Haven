# ADR 0333：Tools runtime composition 与更新编排归 coordinator

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` runtime composition、PlatformRuntime 更新、MCP discovery index 与 builtin catalog rebuild
- 关联：[ADR 0211](0211-operation-registry-and-platform-snapshot.md)、[ADR 0326](0326-tools-runtime-capability-resolution.md)、[ADR 0331](0331-tools-manager-capability-snapshot.md)、[ADR 0323](0323-model-config-apply-coordinator-ownership.md)、[ADR 0324](0324-settings-apply-phase-failure-observability.md)

## 背景与不变量

ADR 0326/0331 已将 runtime capability 策略和单一 typed snapshot 收口，但 `ToolsManager` 仍创建
`ToolCore`、`ToolRuntime`、`ToolBuiltins`，并直接负责 startup wiring、平台 runtime 更新、MCP config/index
更新和 builtin catalog rebuild。配置 apply、MCP 连接与 rebuild 分散在 app-binary commands、
`RuntimeConfigCoordinator`、`ToolsManager` 与 `McpManager`，维护者难以从单一位置判断工具侧更新顺序和失败边界。

本切片保持以下不变量：

1. `PlatformRuntime` 仍由 `RwLock<Arc<_>>` 整份原子替换；读者继续持有已读取的 generation。
2. MCP 连接与健康监控仍归 `McpManager`；其 `catalog_version` 仍按原路径递增。MCP config/index 的工具侧投影顺序不变。
3. Provider web-search 优先级保持 provider → MCP → unavailable；媒体、STT/TTS、recording、工具授权与执行前校验保持原有读取和 gate。
4. 每次能力读取继续从同一 `ToolCapabilitySnapshot` 构造路径解析，不新增 snapshot、副本缓存或组合版本钟。
5. catalog rebuild 注册冲突仍记录相同错误文本并保留上次成功发布的 registry/deferred/catalog；平台配置失败或降级策略不变。
6. ApplicationRuntime 的退出顺序、工具运行时线程安全、错误文本和 Tauri IPC 均不变。

## 决定

1. 新增 crate-private `ToolRuntimeCoordinator`，由它创建并持有 `ToolCore`、`ToolRuntime` 与
   `ToolBuiltins`，并实际实现 startup wiring、Router/media runtime publish、security/tool/context/shell
   更新、MCP config load/discovery/upsert/remove、MCP prompt index 构造以及 scoped catalog rebuild。
   `ToolsManager` 留作对外 façade，公开入口转发更新操作，并继续承担 execution/authorization、
   session Skill/MCP overlay、managed asset lease、catalog projection、runtime capability 请求和
   recording transcription。
2. 原更新阶段和顺序原样移入 coordinator：
   - startup 仍先绑定 messaging/memory ports，再设置 limits/security/tool settings/action store，替换
     platform snapshot，最后 rebuild catalog；绑定失败仍在 runtime/catalog 发布之前返回原错误。
   - Router/media 更新仍先替换整份 platform snapshot，再按 media/files/window roots rebuild。
   - security 仍先更新 authorization 与 MCP network policy，再更新 platform snapshot，不触发 catalog rebuild。
   - context/tool settings/default shell 更新与依赖服务的先后、受影响 root 范围和 settings 改动不清除 session grants 的行为均不变。
   - catalog rebuild 仍先准备完整 builtin 列表；installed registry rebuild 失败就退出，不替换 deferred 或
     `BuiltinCatalog`，成功后按原顺序发布 deferred/catalog 并 bump global version。
3. 当前没有覆盖 app config commit、Router prepare、PlatformRuntime publish、MCP 连接与 catalog refresh 的
   单一原子入口，因此 coordinator 只拥有 tools crate 内 composition/update orchestration。app-binary
   `RuntimeConfigCoordinator` 继续持有 config apply gate 并准备 Router/media clients；`update_settings`
   继续编排 settings 多阶段；MCP Tauri 命令继续持久化配置并决定连接、刷新、失败或移除动作，然后调用
   ToolsManager façade 更新配置投影/重建目录；`McpManager` 继续拥有实际连接、monitor 与 MCP
   `catalog_version`。这些边界未提供跨来源原子发布，故不引入共同缓存或版本。
4. `AuthorizedExecutor` 与 builtin/tool 业务逻辑不迁移。Coordinator 不自行实现 shutdown；
   `ApplicationRuntime` 仍负责取消应用任务、经 app-owned `MemoryStartup` 停止共享 Agent worker、关闭 input pipeline、清理会话、关闭
   actions 和 MCP clients，最后 join app-owned tasks。

## 替代方案

- 继续将组合和更新编排留在 `ToolsManager`：保留多个独立职责混在 façade 中的现状，拒绝。
- 将 app-binary config gate、MCP 连接或完整应用 shutdown 搬入 `haven-tools`：需要跨层反向依赖或改变
  资源生命周期，拒绝。
- 缓存 `ToolCapabilitySnapshot` 或假设 MCP 与 Router/platform 共用版本：当前不存在完整失效时钟，拒绝。

## 影响与验证

- 不改变 schema、配置格式、IPC、provider tool contract、MCP 连接协议或授权语义；无需数据重置。
- Coordinator 单测覆盖 platform publish/catalog rebuild、startup binding 失败时 platform/catalog 不发布、
  MCP config/discovery 更新与 index/catalog version 刷新顺序。并发 capability 读者与协调更新/catalog
  rebuild 并行；应用 shutdown 测试覆盖 MCP index/capability 读取与既有关闭过程并行。
- 已有 `RuntimeConfigCoordinator::failed_settings_prepare_is_before_router_publish` 保持 config
  prepare 失败时不发布 Router/runtime 的边界。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、
  `cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、
  `cargo test --workspace --locked`、`git diff --cached --check`。

## 回滚

将 `ToolRuntimeCoordinator` 中的 composition、更新和 catalog orchestration 恢复到 `ToolsManager`，并
删除本 ADR、索引及 architecture/roadmap 记录即可。无 schema、配置、IPC 或用户数据迁移。
