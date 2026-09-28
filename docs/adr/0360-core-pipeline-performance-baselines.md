# ADR 0360：核心流水线性能基线入口与测量边界

- 状态：已采纳（2026-09-26）
- 范围：SessionActor mailbox、durable event replay、UI reducer broadcast、Action completion outbox、Memory fact extraction outbox
- 关联：ADR 0178（ReAct 可观测性）、ADR 0359（profiling 基线审计）、ADR 0389（事件保留与容量边界）

## 背景

ADR 0359 确认工作区没有 Criterion 等基准框架，并列出五条流水线已有的行为测试和观测缺口。`active_replay_boundary_benchmark_1k_10k_100k` 已有合适的内存数据库夹具，但此前每种历史规模只测一轮，且以毫秒输出；默认 test harness 不安装 tracing subscriber，因此看不到其 tracing 行。

## 决定

1. 只增强已有 replay fixture，不加依赖、不改生产代码。对 1k、10k、100k 条历史 transcript event，每种规模在同一内存 SQLite fixture 上分别预热 full/active read 各 2 次，再交错采集 21 对样本。fixture 构建和写入不计入样本；样本边界是一次 `read_all` 或 `read_active` 调用直到返回，包括 SQLite 查询、行映射、active-event payload 分类和返回值分配。
2. `cargo test --locked -p haven-memory active_replay_boundary_benchmark_1k_10k_100k -- --nocapture --test-threads=1` 输出每种规模的成对样本数、历史输入规模、active 返回事件数，以及 full/active read 的微秒 nearest-rank p50/p95。该入口在 Cargo test profile 运行；21 个样本的 p95 是粗粒度观察值，不设置通过阈值，也不代表磁盘读取、冷缓存、启动恢复或生产并发延迟。
3. 其余四条链路继续由既有行为测试复跑；当前夹具无法在不改变调度或混入其它成本的前提下分离用户关注的阶段时间，所以本轮不新增计时器或合成 benchmark。具体缺口与命令如下。

### 2026-09-26 单次基线运行记录

命令使用上一节的单测入口。环境为 Rust 1.98.0、Windows 11 Insider Preview `10.0.29671`、Intel Core Ultra X7 358H；Cargo `test` profile（unoptimized + debuginfo），内存 SQLite，无网络。以下 p50/p95 以每个读模式 21 个样本按 nearest-rank 计算：

| 历史 transcript events | full 返回数 | active 返回数 | full p50/p95 (us) | active p50/p95 (us) |
|---:|---:|---:|---:|---:|
| 1,000 | 1,002 | 2 | 1,548 / 1,659 | 64 / 93 |
| 10,000 | 10,002 | 2 | 15,331 / 15,836 | 169 / 230 |
| 100,000 | 100,002 | 2 | 153,273 / 155,927 | 203 / 283 |

这是单机单次入口运行，用作之后在相同 fixture/profile 下复跑的参考，不作为跨机器阈值或生产延迟估计。

| 链路 | 复跑入口 | 当前可观察 | 尚缺且未测量 |
|---|---|---|---|
| SessionActor mailbox | `cargo test --locked -p haven-agent session::actor::queue_tests::actor_services_external_commands_while_run_handler_is_awaiting_provider` | actor 在 ReAct handler 等待期间继续处理外部命令的行为；性能设置导出的 `context_queue_items` 是 ReAct 上下文队列，不是 ActorCommand mailbox | 命令入队等待、队列深度/高水位、dequeue→处理耗时分布 |
| Session event replay | 上述 `active_replay_boundary_benchmark_1k_10k_100k` 命令 | 每个 fixture 的 full 与 compact-summary active suffix 读取 p50/p95；生产 `read_active` 仍有 `scan_ms` debug 字段 | 内存 fixture 不覆盖持久磁盘、冷缓存、回滚 fallback、完整 session startup/resume 时间 |
| UI reducer broadcast | `corepack pnpm --dir ui run test:run -- src/lib/sessionReducer.test.ts src/lib/sessionSelectorStore.test.ts` | dispatch 通知次数、selector 相等性和订阅释放行为 | dispatch elapsed、活跃 subscriber 数、每次广播通知数及大 transcript 的 CPU/内存成本 |
| Action completion outbox | `cargo test --locked -p haven-tools action_service::tests`；`cargo test --locked -p haven-memory action_completion_outbox` | reconcile、claim、CAS、ack、重试与 late-attach 恢复行为 | enqueue/claim/commit/ack 阶段时延、pending depth、最老条目年龄、drain throughput |
| Memory fact extraction outbox | `cargo test --locked -p haven-agent memory_worker` | durable marker、恢复、coalescing、retry/backoff 与 cancellation 行为 | pending depth、enqueue→claim→persist 时延、队列年龄和吞吐；provider 调用耗时也没有分离 |

这些行为测试的总运行时不作为基线。已有 LLM/ReAct phase metrics 是固定桶估算，provider/network 等待不等于 crate-local 成本。读取/导出的计时结果必须继续附场景、样本数、输入规模、profile、单位和边界；没有合适夹具时应报告缺口，不推断延迟。

本 ADR 的 100k 内存 SQLite replay 基线不定义事件保留期限或存储容量。当前 session 事件保留与尚未覆盖的磁盘增长、容量告警和低磁盘 durable append 行为见 ADR 0389。

## 影响与验证

- 生产 actor、存储、outbox 与 reducer 的调度、顺序、取消/重试和 wire/storage 契约不变。
- 无新增依赖、指标或 runtime policy；replay 计时代码只存在于已有 Rust 测试。
- 验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`；单独复跑命令会打印 replay baseline 行。

## 回滚

删除本 ADR 与 replay 测试中的输出/采样循环即可；无数据库、配置、IPC 或用户数据重置要求。
