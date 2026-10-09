# ADR 0836：集中聊天模型命令 owner

## 状态

Accepted — 2026-10-09

## 背景

Chat toolbar 的 `switch_model`、`set_reasoning_effort` 和 `set_web_search` 由 `chatModelOperations` 通过注入的通用 invoke 发出。配置同步模块 `chatModelSync` 又直接 invoke `set_web_search`，用来清理当前模型不支持或不接受的旧值。同一 web-search 命令因此跨两个 owner 发出，`chatModelOperations` 也重复声明了 generated command invoker 的局部接口。

## 决定

- 新增 `chatModelCommands.ts`，作为上述三个 Chat 模型命令的唯一 Tauri `invoke` owner，request 类型复用 generated command contract。
- `chatModelOperations` 依赖该模块导出的命令集合，不再声明重复的 invoke overload 类型，也不直接拼命令名与 payload。
- `chatModelSync` 通过同一个 `setWebSearch` wrapper 规范化持久设置；`+page.svelte` 只把命令 owner 注入操作控制器。
- IPC 门禁登记这三个命令的 owner，禁止其他 UI 源文件直接调用它们。
- 保留 profile 切换、成功后更新 toolbar、Gemini `always`→`auto`、不支持时清理旧值、错误呈现及失败后重置 refresh suppression 的现有行为。

## 替代方案

- 继续由两个模块分别提交 `set_web_search`：拒绝。同步与用户选择触发时机不同，但命令 wire owner 相同；将它们收敛到一个 adapter 不改变各自控制流程。
- 将模型操作业务逻辑移入命令模块：拒绝。model selection、toolbar 状态、通知和错误恢复仍由 `chatModelOperations` 拥有；adapter 只转发命令契约。
- 保留局部 `ChatModelOperationsInvoke` overload：拒绝。它重复描述同一组 generated request shapes，容易随 command contract 漂移。

## 影响与验证

仅收口 UI 内部命令 owner，不改变 Rust IPC、配置 schema、provider capability 规则或持久化，无需重置。验证：UI 类型检查、相关控制器/同步/命令测试、UI 全量测试、生产构建、`scripts/check-ipc-contracts.ps1`、Prettier 和差异检查。

## 回滚

恢复 `chatModelOperations` 的注入 invoke 与 `chatModelSync` 的直接 invoke，删除 `chatModelCommands.ts`、其测试、ADR 和 IPC owner 规则；无需数据库或配置重置。
