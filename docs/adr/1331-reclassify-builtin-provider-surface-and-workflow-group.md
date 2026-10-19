# ADR 1331：重分类内置工具直调面与工作流目录

## 状态

Accepted — 2026-10-19

## 背景

Builtin 的常驻 provider surface 只包含 Ask、通知和目录/加载入口；Shell、HTTP 和系统概览也必须先经过目录加载。与此同时，Agent、Session、Schedule 与 ToolRun 操作在设置页和目录摘要中分散于 `agent`、`haven` 等分类。

## 决定

1. 每轮直接提供以下 builtin 工具：`ask`、`notify`、`shell`、`http`、`system.info`、`tool_catalog`、`load_skill`、`load_mcp`。`tool_catalog` 是加载其它 deferred builtin operation 的入口。
2. 其它 builtin operation 保留在 deferred catalog，继续通过 `tool_catalog(action=load)` 按 operation 或 root 加载到当前 session。Skill 和 MCP 的既有 loader 行为不变。
3. 新增 `workflow` 目录组，将 `agent.*`、`session.*`、`schedule.*` 与 `toolrun.*` 操作归入其中。Haven 管理与记忆、文件系统、Desktop、System、Media、Network、Shell 保持独立分组。
4. `ToolCall` 是 ReAct 前台执行路径，不是独立工具根；当前没有 `toolcall.*` builtin operation。后台调用与管理仍由 `toolrun.*` 提供。
5. 目录分类及 provider 直调/按需加载只控制可见性和路由，不改变工具名、权限 key、风险、确认、参数 schema、安全网关或执行边界。

## 替代方案

- 保留全部 builtin operation deferred：拒绝；Shell、HTTP 和系统概览不能从每轮 provider tool list 直接调用。
- 将全部 Haven 操作归入 `workflow`：拒绝；Memory 与 Haven 管理功能和 Agent/Session/Schedule/ToolRun 协作流程是不同用户任务。
- 新增虚构的 `toolcall.*` 目录 operation：拒绝；ToolCall 是执行路径术语，不是现有注册工具命名空间。

## 影响与回滚

`ToolCatalogGroup` 的 wire 值由 `agent` 改为 `workflow`，Rust 与 UI 必须同版本发布并同步生成 TypeScript 契约。无数据库、配置或 durable event 变化，无需重置。授权和执行逻辑不变。回滚时恢复旧目录组和原 provider 白名单，并重新生成 UI 契约。

## 验证

验证覆盖核心 registry/provider surface、deferred operation 加载、Workflow 目录分组、提示词中的跨会话说明、UI 分类展示与 IPC enum 漂移：

- 通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`。
- 通过：`corepack pnpm run check`、`corepack pnpm run test:run`（162 files / 1345 tests）、`corepack pnpm run build`、IPC contract check、ADR index check。
- `check-ipc-events.ps1` 报告 `crates/app-binary/src/bootstrap.rs` 有 7 处未登记事件发射点；本变更未触及事件生产或投影代码。
