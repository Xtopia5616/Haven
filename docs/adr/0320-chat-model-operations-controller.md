# ADR 0320：聊天页模型操作归属到 ChatModelOperations

- 状态：已采纳（2026-09-25）
- 范围：聊天页模型、reasoning effort 与 provider web-search 操作
- 关联：[ADR 0313](0313-chat-controller-session-orchestration.md)、[ADR 0315](0315-chat-event-registration-controller.md)、[ADR 0055](0055-ui-chat-model-sync-boundary.md)

## 背景

`+page.svelte` 的三个模型 toolbar handler 各自调用 Tauri、修改页面状态、发通知并报告错误；默认模型 discovery/settings 同步则已由 `chatModelSync.ts` 拥有。命令编排与同步归属因此分散在页面和模块之间。现有 `contracts/commands.ts` 为这些命令登记了请求 DTO 名称，但没有对应的 TypeScript payload shape。

## 决定

- 新增无 Svelte、DOM 或页面生命周期依赖的 `chatModelOperations.ts`，拥有 `switch_model`、`set_reasoning_effort`、`set_web_search` 三个聊天页操作的 payload、成功状态更新、通知、失败上报和 refresh-suppression 回调。
- Controller 通过注入的 typed invoke 和 callback dependencies 工作；测试使用纯内存 callback，不启动 Tauri。`+page.svelte` 只连接 Svelte setter/getter、notification/error callback、菜单关闭 callback，并把 controller handlers 传入 `ModelToolbar`。
- 在 `contracts/commands.ts` 补充这三个 command 的最小 renderer payload interface。命令目录仍是手写镜像；本决定不引入 Rust DTO 到 TypeScript 的生成流程，也不改 Tauri 命令名、字段或 wire shape。
- `chatModelSync.ts` 继续拥有默认模型 discovery 和 settings 同步；`chatEventController` 继续消费 `skipNextDefaultModelRefresh`。Controller 只写入该既有 boolean callback，不改变 flag 语义。

## 必须保持的不变量

- 每次受支持的模型、effort 或 web-search 操作均先将 suppression flag 置为 `true`，命令成功后更新对应页面状态并发出原成功通知；配置变更事件仍由 event controller 消费该 flag。
- invoke 失败时将 flag 复位为 `false`，使用原 context `+page`、错误文案与 `log: false`。
- 模型选择在 invoke 前关闭菜单，模型显示名仍 fallback 到 id；effort 空值在线上仍为 `null`、页面值仍为空字符串。
- provider 不支持内置 web search 时，非 `off` 模式只发送原 info 通知并结束，不调用命令、不改 flag；`off` 仍允许发送。
- Gemini 的 `always → auto` normalization 仍属于 settings sync；toolbar 操作发送所选原值。并发请求继续共享现有 boolean flag，并按完成顺序写入页面状态；不在本切片加串行化或并发计数。

## 替代方案

- 继续把 handler 留在页面：会让 command payload、状态更新和失败保护继续散落，拒绝。
- 把 settings discovery 或 Gemini normalization 一并迁入 operations controller：会合并两个已分离的模型职责，超出本切片。
- 扩大为 Rust DTO/mapper 生成改造：范围更大，留待 Phase 8 后续独立完成。

## 影响与验证

- UI 单测覆盖 model/effort/web-search 成功、三类操作失败后的 suppression reset、unsupported web-search 拒绝，以及不支持时仍可关闭；均使用注入 invoke，不依赖真实 Tauri。
- 不涉及 Rust handler、IPC、持久化或 provider normalization 变化，无需数据重置。
- 验证命令：`cd ui; corepack pnpm run check`、`cd ui; corepack pnpm run test:run`、`cd ui; corepack pnpm run build`、`cargo check --workspace --locked`、`git diff --cached --check`。
- Rust DTO → TS contract/mapper generation 与旧手写 contract 镜像清理仍是 Phase 8 后续工作。

## 回滚

回滚本提交即可恢复 `+page.svelte` 中的三个 toolbar handler；移除最小 payload interfaces、测试、本 ADR 与路线图/索引更新。无需数据迁移或重置 IPC。
