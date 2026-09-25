# ADR 0359：Common crate 边界与 profiling 基线审计

- 状态：已采纳（2026-09-26）
- 范围：Rust workspace 依赖边界、`haven-common` 模块所有权与下一轮性能剖析基线
- 审查基准：HEAD `5378013`；审查前工作区干净
- 关联：ADR 0178（ReAct 可观测性）、ADR 0356（路线图状态核对）

## 背景与证据

`cargo metadata --format-version 1 --no-deps --locked` 和 `cargo tree --workspace --edges normal --depth 1` 显示，审查时 workspace 有 9 个 crate；`haven-common` 没有内部依赖，被 8 个 crate 直接依赖。Cargo metadata 中所有 workspace crate 的 feature 集合为空；唯一带 `custom-build` target 的包是 `haven-app-binary`，其 `build.rs` 调用 Tauri build 并登记图标重跑输入。工作区没有 `benches/`、`[[bench]]`、Criterion、Divan、Iai 或采样 profiler 配置。

`haven-common` 的 `lib.rs` 公开了 config、media、media detection、types、prompts、text、encoding、tools、lifecycle、hooks、error、workspace、action lease 与 process containment 模块，并重导出多个跨层契约。静态引用显示这些模块多数仍被多个领域消费：例如 prompts 由 Agent、LLM 和 Tools 共用；media/types 进入 Agent、App、LLM、Memory、Tools 的边界；`ConfigService` 同时由 app runtime/settings 和 Tools admin 使用；媒体探测同时用于 App attachment ingress 与 Tools media/file paths，`MediaType` 也是 `MediaAsset` 的契约字段。因此本轮不把这些域按名字拆成新 crate。

唯一满足窄所有权条件的适配是 `ProcessContainment`：Windows Job Object 的 OS handle 封装原在 common，并是 common 唯一的 `windows-sys` 使用点；MCP stdio client/transport 与 Tools 的 Shell、Skill、后台 Action 是两个独立子进程 owner。把它移入任一现有业务 crate 会让另一个 owner 依赖错误的业务层，或造成反向依赖。独立 `haven-platform` 可作为无内部依赖的操作系统叶子，不重复类型或策略。

性能方面，项目已有 ADR 0178 建立的有界 ReAct 观测：固定内存直方图按 phase 导出 `count`、`total_ms`、桶化 `p50_ms`/`p95_ms`，另有失败/重试计数与 `context_queue_items` gauge；Settings 的 `get_performance_metrics` 将后端快照与 UI 的 `frames`、`chunks`、`drops` 一起导出。它不代表所有本轮目标均已覆盖。`SessionEventStore::read_active` 有 `scan_ms` debug 字段，transcript commit 暴露 SQLite lock wait，且已有 `active_replay_boundary_benchmark_1k_10k_100k` 单测。该测试命令本轮通过（1 项，测试 harness 总耗时 2.47 秒）；默认 test harness 没有 tracing subscriber，所以此次没有输出 full/active 单次读取数值，该总耗时不作为性能基线。

其它目标当前主要由行为测试覆盖：actor mailbox 没有 command queue wait/depth 直方图；reducer 测试固定订阅通知和 selector 相等性行为，但没有广播成本计时；LLM 指标可观察 request context、first token 和 stream phase，mock 测试不代表真实 provider/network latency；Action completion 与 Memory durable outbox 有恢复、重试、CAS 和 acknowledgement 测试，但没有端到端延迟或 backlog 时序指标。

## 决定

1. 新增无内部依赖的 `haven-platform`，只迁移 `ProcessContainment`。MCP 与 Tools 分别直接依赖它；Common 不再包含 OS handle/Windows FFI，也不再声明 `windows-sys` target dependency。
2. `ProcessContainment` 的实现原样迁移：Windows 仍用 kill-on-close Job Object，`new`/`attach(pid)` 失败仍返回原 I/O 错误，调用方仍负责失败时终止刚启动的子进程；非 Windows 的 no-op 行为和各 adapter 的创建、取消、drop 时机不变。只有内部 Rust import path 从 `haven_common` 改为 `haven_platform`；Tauri IPC、provider wire、事件、数据库、ID 与 X12 均不变。
3. 继续让 Common 持有多 crate 共用的数据、纯函数和配置契约。当前不拆 config、media、types 或 prompts；缺少可证明的单一新 owner，额外 crate 只会移动复杂度或要求新增服务层依赖。
4. 下一轮 profiling 先采集同一 fixture 下的既有快照，再补齐未覆盖的计时；在拿到分布前不新增 cache、selector 或 batching。

## 下一轮基线命令、指标与边界

| 区域 | 复跑命令/入口 | 当前可观测值 | 尚缺指标或边界 |
|---|---|---|---|
| 依赖与平台边界 | `cargo metadata --format-version 1 --no-deps --locked`；`cargo tree --workspace --edges normal --depth 1`；`pwsh -NoProfile -File scripts/check-crate-dependencies.ps1` | crate/target/features、直接依赖边 | package graph 不等同编译时间；如比较构建时间，需固定 toolchain、目标与冷/热 cache 条件并重复采样 |
| SessionActor mailbox | `cargo test --locked -p haven-agent`；运行固定会话场景后经 Settings 导出 `get_performance_metrics` | 已有 context queue item gauge；相关 mailbox 行为测试 | context queue 不等于 ActorCommand mailbox。需 command enqueue→处理耗时分布、队列深度/高水位与 backpressure 计数 |
| durable event replay | `cargo test --locked -p haven-memory active_replay_boundary_benchmark_1k_10k_100k -- --nocapture` | 1k/10k/100k in-memory 历史，full/active read 的测试计时字段；生产 `scan_ms` 与 transcript SQLite lock wait | 本轮执行没有打印 tracing 样本；需让 harness 明确输出每种历史大小的重复样本/p50/p95，并覆盖 rollback fallback 和 compaction suffix。该内存测试不能代表磁盘、冷 cache 或启动恢复总时长 |
| UI reducer/broadcast | `corepack pnpm --dir ui run test:run -- src/lib/sessionReducer.test.ts src/lib/sessionSelectorStore.test.ts` | dispatch 通知、selector 通知/释放的行为断言 | 无 reducer dispatch elapsed、活跃 subscriber 数、每次广播通知数或大 transcript 下的 CPU/内存分布 |
| LLM request | `cargo test --locked -p haven-llm`；固定 ReAct 场景后从 Settings 导出性能快照 | ReAct `request_context`、`first_token`、`llm_stream` phase histogram；turn/chunk/drop counters | phase 不能分离 route/permit wait、connect/TLS、provider 首字节及 response decode；mock tests 不含真实网络，不将网络等待归为本地 crate 成本 |
| Action completion outbox | `cargo test --locked -p haven-tools action_service_tests`；`cargo test --locked -p haven-memory action_completion_outbox` | retries/duplicates counter 与当前 CAS、reconcile、ack 行为测试 | 缺少 enqueue/claim/commit/ack 各阶段时延、pending depth、最老条目年龄及 drain throughput |
| Memory durable outbox | `cargo test --locked -p haven-agent memory_worker` | retry/backoff、恢复 marker 和 worker cancellation 行为测试 | 缺少 pending marker 数量、enqueue→claim→persist 时延、重试队列年龄/吞吐；不把 provider LLM 延迟与本地 outbox 成本混算 |

`MetricsSnapshot` phase 百分位是固定桶的上界估算，不是原始样本，也不提供 p99。现有指标是本地有界快照，不是持续时序存储或跨运行比较器。性能数据必须附场景、样本数、fixture 大小、debug/release profile 和是否经过真实网络；在此之前只记录结构证据与行为测试结果。

## 影响与验证

- workspace crate 数量增加 1；新内部边只有 `haven-mcp → haven-platform` 与 `haven-tools → haven-platform`。`haven-platform` 不依赖 common 或业务 crate，不形成循环。
- `haven-common` 的 Windows target dependency 被移除；Windows Job Object 实现仍由 `haven-platform` 的 Windows target dependency 提供。没有宣称总 workspace 编译时间已下降。
- 代码迁移仅改变 Rust 模块归属与 import path；上述 OS 行为和 IPC/storage/runtime 契约保持。
- 验证命令：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`pwsh -NoProfile -File scripts/check-crate-dependencies.ps1`、`git diff --check`。
- 本轮结果：以上命令全部通过；event replay 基准筛选测试 1 项通过。没有运行 UI 门禁，因为 UI 与 IPC 均未改动。

## 回滚

回滚该提交，将 `process_containment.rs` 移回 common、恢复其 Windows target dependency，并还原 MCP/Tools imports、workspace dependency allow-list 和架构文档。无需数据、schema、配置或 wire 重置。
