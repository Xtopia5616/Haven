# ADR 1301：收敛 App capability、ToolRun payload 与 IPC metadata owner

状态：已接受并实施（2026-10-17）。

## 背景

App composition root 将 Tools 已提供的 ToolRun management 与 live-output capability 再包进同名转发接口；test-only schedule 也经由生产 wrapper 暴露。UI 全局任务面板和 session history 各自持有一份相同 `ToolRunPayload`，虽然两者需要不同的缓存、查询和淘汰生命周期。Tauri command boundary/security 文案在 Rust、TypeScript 和 IPC 文档中手工复制，且现有 checker 只核对名称。AgentEvent 的 channel 与 payload DTO 由两个 exhaustive match 分开选择，配对关系没有和生成的事件 shape registry 核对。

## 决定

1. `AppServices` 直接持有 Tools composition root 提供的窄 `ToolRunManagementCapability` 与 `LiveOutputSinkPort`；删除同名转发 adapter。测试 schedule 只通过 test-support capability 调用。
2. UI `toolRunState` 是 ToolRun payload 的唯一 owner。全局 live board 与 session timeline 分别维护 bounded ID index，两个既有只读 store 是其派生视图。保留 stale refresh 防护、live row 保护、每 session 200 条上限和 16 个 session cache 上限。
3. `COMMAND_CONTRACTS` 是 command boundary/security metadata 的唯一输入。IPC generator 从该 Rust registry 生成 TypeScript metadata 与 `docs/ipc-contracts.md` 表格，并要求 registry 名称与 Tauri handler 集合相同；`--check` 比较生成文件和文档表的完整内容。
4. AgentEvent 通过一个 exhaustive match 生成 `{ channel, payload }`。IPC generator 同时产出 channel→payload DTO registry；`check-ipc-events.ps1` 将 Rust projection 中的 channel/DTO 对逐项与生成 registry 比较。
5. 这些调整不改变 Tauri payload、event 顺序、权限策略、持久数据或 ToolRun lifecycle 契约。

## 替代方案

- 保留 App 转发 trait 以维持表面隔离：拒绝。现有 Tools capability 已经是 App 需要的窄接口，转发没有转换、校验或独立策略。
- 在两个 UI store 中继续存完整 payload：拒绝。两个 store 可保留查询边界和容量策略，但同一实体的 payload 只由一个 map 拥有。
- 依赖 reviewer 手工比对 metadata 或只比较命令名称：拒绝。生成和 drift 检查必须覆盖 boundary、安全文案及事件 channel/DTO 配对。

## 影响与恢复

IPC 输出形状、事件 channel、配置和数据库均不变，无需数据库或 inbox 重置。UI 的 `toolRunStore` 与 `sessionToolRunStore` 仍提供原有读取形状，现在为只读派生 store；仅测试改用内部 reset hook。回退代码并从 Rust registry 重新生成 IPC 文件即可恢复之前实现，不涉及用户数据转换。

## 验收

- `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 通过。
- `scripts/check-ipc-contracts.ps1` 验证 85 个 handler 的注册、生成 TypeScript、boundary/security metadata、文档和完整 UI/non-UI disposition 一致；`scripts/check-ipc-events.ps1` 验证 36 个 channel 与 AgentEvent channel/DTO 配对。
- `corepack pnpm run check`、`corepack pnpm run test:run`（161 files / 1336 tests）及 `corepack pnpm run build` 通过；随后新增的共享 payload 刷新竞态回归以 `vitest run src/lib/toolRunStore.test.ts`（16 tests）通过。
- UI 测试覆盖 command metadata drift、单一 payload identity、live board 移除后 session history 保留、history hydrate 与并发 stale response。Rust workspace 所有单元、集成和 doctests 均通过。
