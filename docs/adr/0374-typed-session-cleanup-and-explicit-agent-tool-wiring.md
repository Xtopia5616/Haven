# ADR 0374：Session 清理 typed ports 与 Agent 工具显式 wiring

- 状态：已采纳（2026-09-26）
- 基线：HEAD `9f0c34f`；开始时工作区干净
- 范围：`AppState` 后台 session/media 清理、`SessionStore` 清理端口，以及 Agent 内部 `ToolsManager` service locator 访问
- 关联：ADR 0224、0225、0237、0257、0291、0295、0361、0363、0364、0365、0366、0373

## 背景与审计结论

`AppState` 的启动恢复、历史保留和上传引用清理已经调用现有的 typed repository 操作，
但后台 task 仍捕获 raw `Arc<Database>`。这使低层句柄越过组合根进入长期异步任务，且清理路径没有
自己的 `SessionStore` port 回归覆盖。

`SessionSupervisor` 同时持有实际执行所需的 `ToolsManager` 与完整 `ToolServices` bundle。
AgentLayer 通过 `executor.get_tools()` 反向取得 manager，Agent 内部多个路径还通过通用
`executor.services()` 取得 authorization/actions。这些调用没有跨 crate 的业务语义，但会把
service locator 形状继续扩散到 Agent wiring。

## 决定

1. `SessionStore` 增加异步 typed ports：
   `finalize_orphaned_running_sessions`、`delete_old_sessions` 和
   `list_managed_attachment_paths`。它们只通过现有 `Database::run_blocking` 调度既有 repository
   操作，不复制 SQL、事务或缓存失效逻辑。
2. `AppState` 的启动、一次性 retention、upload retention 和每日 cleanup task 只捕获
   `SessionStore` clone。原有日志、错误降级、保留期、媒体引用筛选和 task cancellation 语义保持。
3. `SessionSupervisor` 内部保存 `AuthorizationEngine` 与 `ActionService` 的窄 capability，
   删除生产路径的通用 `services()` 和 `get_tools()` 访问。`tools` 仍由 supervisor 私有持有，
   因为 tool runner 和 session adapters 仍需要真正的执行/目录 facade；本 ADR 不假装已经完成
   `ToolExecutionContext` 或完整 execution port 的拆分。
4. `AgentLayer::build` 从组合根显式接收共享 `Arc<ToolsManager>`。目录 adapter、prompt builder
   和 ReAct engine 继续使用同一实例，避免创建第二个 catalog、authorization 或 runtime owner。
   这是 wiring 变化，不改变工具目录 snapshot、授权确认、执行、取消、重试、X12 或 UI 行为。
5. 所有清理 port 继续遵守 `run_blocking` 的既有取消语义：future 被 drop 不会中断已经进入
   Tokio blocking pool 的 SQLite 操作。调用方仍拥有 task 取消、日志和失败降级策略。
6. Terminal history 删除与 completion outbox 共用 SQLite writer 原子边界：删除前先补齐可能缺失的
   background completion outbox，并仅在所有关联 completion 已 ack（`delivered_at` 非空）时删除。
   未确认 completion 拒绝删除；ack 与 delete 并发时不允许出现“删除成功但 ack 失败”的结果。

## 保留边界与明确不做

- 组合根仍可持有 raw `Database`，用于创建 `SessionStore`、`MemoryService` 和其它 typed owners；
  `MemoryService` 的私有 backing handle 与 repository 内部 raw connection 不是本 ADR 的上层穿透。
- `SessionSupervisor` 的执行 facade、`SystemPromptBuilder`/tool catalog/observation adapters
  的 `ToolsManager` 依赖仍保留。未来只有在明确 execution/authorization 生命周期与替换测试边界后，
  才按 ADR 0224/0225 单独引入新的 port。
- 不改 Settings recovery、完整 Job lifecycle、统一后台/定时任务 UI 投影或全局 IPC codegen；
  Settings、Job watcher/deadline 和统一 UI 投影沿用各自独立切片。terminal history 的未确认
  completion guard 属于本 ADR；其它 Action lifecycle 语义沿用 ADR 0373。
- 不改 schema、配置、ID、wire、X12、数据库重置或用户数据。

## 回归与验证

`SessionStore` 回归覆盖 typed cleanup port 保留孤儿运行会话转 `Error` 的语义，及 retention 删除
的计数/最终空库结果。ActionService 回归覆盖未 ack completion 的拒绝删除、ack 后删除，以及
ack/delete writer race。Agent 既有 catalog、授权、resume、action completion 和 tool execution
测试继续通过显式 manager wiring 构造。验证命令：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-memory
cargo test --locked -p haven-agent
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

## 回滚

回退本 ADR 关联的 wiring、typed port、测试和文档即可；不需要数据库、配置或用户数据重置。
