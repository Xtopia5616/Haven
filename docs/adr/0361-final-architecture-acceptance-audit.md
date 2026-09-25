# ADR 0361：全局架构完成定义最终验收审计

- 状态：已采纳（2026-09-26）
- 范围：路线图全局完成定义、运行时边界、测试隔离、IPC 校验与发布验收入口
- 审查基准：HEAD `3a48213`；审查开始时工作区干净
- 关联：ADR 0349、0351、0352、0356–0360

## 背景

阶段 9 要求在全新数据根目录验证启动、设置、会话、工具、媒体、任务、恢复、回滚、升级重置和卸载；路线图的全局完成定义还要求单一 session owner、窄端口、typed contracts 与可重复门禁。本审计核对代码、现有回归测试、IPC 检查脚本和当前发布/重置流程，不推定未决产品或架构语义。

## 决定与审计结果

| 完成项 | 结果 | 证据与限制 |
|---|---|---|
| durable session transcript 恢复 | 满足，含既定 ingress 例外 | `SessionStore::load_replay_state` 由 `session_events` 生成 transcript、branch points 和 ingress cursor；`resume.rs` 按 event sequence 投影正文。尚未进入事件流的已持久化用户输入只按 ingress cursor/message identity 重新排队；不从 `messages`、`session_steps` 或旧 snapshot 重建 ReAct transcript。恢复与回滚集成测试覆盖 cursor、相同文本不同 message id、截断和失败原子性。 |
| session-local ReAct mutable state owner | 路线图的单 actor-owner 条件满足；ADR 0214 的字段布局尚未对齐 | `SessionActor` task 拥有 `SessionState`、active run slot 与被其持有的 run future；run 创建局部 `ReActState` 并在 actor 的 mailbox `select!` 中继续运行。`ReActState` 是单次 run 的 projection scratch，不与其它 session 共享；当前保存在 run future 内，而不是 ADR 0214 描述的 `SessionState` 字段。`UsageRuntime` 保留用量专属聚合/写入职责。该差异记录为实现对齐事项，不在本审计中改写既有决定。 |
| 上层 raw `Database` / `ToolsManager` 穿透 | 未满足 | 生产构造仍从 `AppState` 将 `Database` 交给 `SessionSupervisor::new`、`AgentLayer::new`，后者交给私有 `MemoryService` 以构造 typed stores；未发现这些上层路径直接用 `conn()` 查询，但边界仍传递 raw handle。Agent 仍从 `SessionSupervisor::get_tools()` 取得 manager（`layer.rs`、`react/mod.rs`），并通过 `services()` 读取进程服务。更大迁移要先明确 Agent 最小执行/context port 与组合根归属。 |
| typed stable outputs 与 dynamic JSON | 部分满足 | session/action/memory/history IPC DTO、usage owner 与 action board typed projection 有命名类型和静态检查。`serde_json::Value` 仍用于 provider 原始载荷、MCP/Skill 与 builtin schema/工具参数和 model-facing tool result；这些保持在 JSON/tool 边界。需要继续逐域确认跨层稳定 ActionService 输出是否都只在该动态边界消费，不做全局替换。 |
| 配置、工具、模型、任务、记忆 runtime 替换/失败/取消测试 | 有局部测试，不足以声明全局语义完成 | Settings/model apply 覆盖有序 phase、Router prepare/publish 失败、durable edit 保留与重复输入不隐式重试；Tools 覆盖 PlatformRuntime 整体替换；LLM Router 覆盖故障、取消与路由；Action 覆盖 CAS、取消、retry、outbox 恢复；MemoryRuntime/Worker 覆盖启动恢复、marker、失败与取消。Settings compensation/rollback/retry/restart、跨 writer 并发和完整 Job lifecycle 仍按 ADR 0351/0352 未决。 |
| Rust/TypeScript IPC | 静态校验通过；全局 codegen 未决定 | `check-ipc-contracts.ps1` 验证 71 个命令以及 Action、Memory、Session history helper 边界；`check-ipc-events.ps1` 验证 40 个 channel。当前 registry/contracts 是手写维护，项目未引入 Rust→TypeScript 全局 codegen；本项的“生成与校验一致”不可记为已满足，是否 codegen 仍是待决策。 |
| 旧 facade、raw invoke、raw `any` 收口 | 仍有真实路径，未发现本轮可独立决定的切片 | 生产仍有 `get_tools()` / `services()` 访问；UI 仍有多处命令直接 `invoke`，以及 Settings/模型视图状态、事件 envelopes 和 dynamic tool JSON 中的 `any`。Memory 与活跃 session history command family 已分别由 ADR 0357/0358 收口；其余命令 family、页面编排和 typed boundary 需继续按域审计，不能推断应统一抽取的 helper 或 codegen 方案。 |

### 临时数据根目录与复跑安全

审计发现 `AppState::new` 接收临时 DB/config 路径，但其启动后台清理任务原先仍使用默认上传 staging 和 `%APPDATA%\\haven\\media\\generated` 根目录。该路径可能删除过期 `file-{uuid}` 生成文件。已增加测试专用 `new_for_test`，其上传与生成媒体 cleanup roots 均位于显式 `TempDir`；生产 `AppState::new` 仍使用原默认路径。

Rust workspace 测试以 `APPDATA=D:\\Workspace\\Haven\\target\\audit-runtime-data-3a48213\\AppData\\Roaming` 运行，目标目录在运行前确认为不存在。配置、Inbox、log、skills 与 generated-media 默认根因此不指向现有用户数据；数据库类测试使用内存 SQLite 或临时文件。没有启动硬编码默认数据根的桌面 bootstrap，没有删除或升级现有数据库。

该代码级隔离不等于完整发布验收。当前没有脚本/fixture 覆盖真实桌面首次启动、安装升级、数据库重置和卸载；这些步骤仍须在一次性 Windows 用户配置或 VM 中人工验收。发布流程不得把 `AppState` 单测通过当作卸载或破坏性重置证明。

## 验证

以下命令均通过；Rust 测试进程使用上述隔离 `APPDATA`：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
git diff --check
```

测试版升级重置与卸载未执行；它们需要隔离的安装/用户 profile harness，目前仓库没有该自动化入口。

## 影响与替代方案

本轮仅调整 AppState 单测的清理根注入，并更新架构/路线图/发布文档。生产清理根目录、schema、IPC、事件、用户配置和 session 语义不变。未选择完整 Job lifecycle、Settings compensation/retry/restart、session cross-channel occurrence identity、Rust→TypeScript codegen 或 command-family/UI orchestration 策略；这些继续由后续 ADR 明确。

替代方案是现在统一拆分 Agent/Tools/App API 或引入全局生成器。本次证据不足以证明其接口和语义选择，因此不作为验收审计的附带改造。

## 回滚

回滚本提交可恢复旧的 AppState 测试 cleanup roots 与审计记录；不需要数据库、配置、IPC 或用户数据重置。
