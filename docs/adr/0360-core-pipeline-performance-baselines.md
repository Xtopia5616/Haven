# ADR 0360：核心流水线性能基线入口与测量边界

- 状态：已采纳（2026-09-26）
- 范围：SessionActor mailbox、durable event replay、UI reducer broadcast、LLM request、Action completion outbox、Memory fact extraction outbox
- 关联：ADR 0178（ReAct 可观测性）、ADR 0359（profiling 基线审计）、ADR 0389（事件保留与容量边界）

## 背景

ADR 0359 确认工作区没有 Criterion 等基准框架，并列出五条流水线已有的行为测试和观测缺口。`active_replay_boundary_benchmark_1k_10k_100k` 已有合适的内存数据库夹具，但此前每种历史规模只测一轮，且以毫秒输出；默认 test harness 不安装 tracing subscriber，因此看不到其 tracing 行。

## 决定

1. 只增强已有 replay fixture，不加依赖、不改生产代码。对 1k、10k、100k 条历史 transcript event，每种规模在同一内存 SQLite fixture 上分别预热 full/active read 各 2 次，再交错采集 21 对样本。fixture 构建和写入不计入样本；样本边界是一次 `read_all` 或 `read_active` 调用直到返回，包括 SQLite 查询、行映射、active-event payload 分类和返回值分配。
2. `cargo test --locked -p haven-memory active_replay_boundary_benchmark_1k_10k_100k -- --nocapture --test-threads=1` 输出每种规模的成对样本数、历史输入规模、active 返回事件数，以及 full/active read 的微秒 nearest-rank p50/p95。该入口在 Cargo test profile 运行；21 个样本的 p95 是粗粒度观察值，不设置通过阈值，也不代表磁盘读取、冷缓存、启动恢复或生产并发延迟。
3. actor mailbox、UI reducer、LLM request 与两个 outbox 使用独立的手动 profile 入口。profile 只存在于忽略执行的测试 harness 或 UI 脚本，不增加生产 timer、外部依赖、性能阈值或 runtime policy；fixture 初始化与 warmup 排除在样本之外。Actor 以预填 `ActorCommand` 队列后执行 Snapshot 往返测入队背压与排队服务时间；UI 以大 transcript 的 `agent/chunks` dispatch 测 reducer、store 与 selector subscriber 广播；LLM 同时测零等待 mock provider 的本地 router 路径及固定 2ms mock service、8 permit 下的排队分布；Action 记录 terminal commit+outbox enqueue、claim/reconcile 与 ack；Memory 记录 durable marker enqueue、固定队列深度扫描与 conditional ack。两个 outbox 使用临时文件 SQLite，经生产 typed store 和 SQLite blocking boundary；不调用外部 provider。测量边界、队列深度与吞吐数据均在本 ADR 的复跑记录中标明。

### 2026-09-26 单次基线运行记录

命令使用上一节的单测入口。环境为 Rust 1.98.0、Windows 11 Insider Preview `10.0.29671`、Intel Core Ultra X7 358H；Cargo `test` profile（unoptimized + debuginfo），内存 SQLite，无网络。以下 p50/p95 以每个读模式 21 个样本按 nearest-rank 计算：

| 历史 transcript events | full 返回数 | active 返回数 | full p50/p95 (us) | active p50/p95 (us) |
|---:|---:|---:|---:|---:|
| 1,000 | 1,002 | 2 | 1,548 / 1,659 | 64 / 93 |
| 10,000 | 10,002 | 2 | 15,331 / 15,836 | 169 / 230 |
| 100,000 | 100,002 | 2 | 153,273 / 155,927 | 203 / 283 |

这是单机单次入口运行，用作之后在相同 fixture/profile 下复跑的参考，不作为跨机器阈值或生产延迟估计。

### 2026-09-29 核心流水线 profile

环境为 Rust 1.98.0、Node.js 24.20.0、Windows 11 Insider Preview `10.0.29671.0`、Intel Core Ultra X7 358H。LLM 与 outbox 使用 `cargo test --release` 的 optimized test profile；actor 因 debug-only test adapter 约束使用默认 debug test profile；UI 脚本由 Node 直接运行。Rust p50/p95 使用 nearest-rank；actor 每个 mailbox 深度采集 129 个 Snapshot 往返样本、预热 8 次；UI 每个 transcript/selector 场景重复 3 次，每轮用 `performance.now()` 采集 513 次同步 dispatch 并预热 16 次，再汇总 1,539 个样本。Rust outbox 通过生产 typed store、SQLite blocking pool 和临时文件数据库运行，fixture/表准备不计入 enqueue 样本。

| 路径与场景 | 样本 | 延迟 p50/p95 | 队列/订阅 | 吞吐 |
|---|---:|---:|---:|---:|
| Actor mailbox：Snapshot roundtrip，depth 0 | 129 | 10.40 / 10.80 us | high-water 0/128 | 94,243/s |
| Actor mailbox：Snapshot roundtrip，depth 32 | 129 | 25.50 / 27.20 us | high-water 32/128 | 38,536/s |
| Actor mailbox：Snapshot roundtrip，depth 64 | 129 | 40.70 / 49.20 us | high-water 64/128 | 23,534/s |
| Actor mailbox：Snapshot roundtrip，depth 128 | 129 | 79.20 / 90.70 us | high-water 128/128 | 12,432/s |
| UI reducer：1k transcript，0 selector subscribers | 1,539 dispatch | 4.80 / 10.30 us | 2 notifications/dispatch | 148,976/s |
| UI reducer：1k transcript，16 selector subscribers | 1,539 dispatch | 3.20 / 4.60 us | 18 notifications/dispatch | 222,569/s |
| UI reducer：1k transcript，64 selector subscribers | 1,539 dispatch | 4.60 / 8.40 us | 66 notifications/dispatch | 154,073/s |
| UI reducer：10k transcript，0 selector subscribers | 1,539 dispatch | 20.10 / 48.60 us | 2 notifications/dispatch | 33,087/s |
| UI reducer：10k transcript，16 selector subscribers | 1,539 dispatch | 20.00 / 33.70 us | 18 notifications/dispatch | 39,815/s |
| UI reducer：10k transcript，64 selector subscribers | 1,539 dispatch | 21.10 / 50.00 us | 66 notifications/dispatch | 31,538/s |
| LLM router：immediate mock provider | 513 requests | 1.10 / 1.20 us | 顺序调用 | 870,820/s |
| LLM router：2ms mock sleep，8 permits、32 requests/batch | 512 requests | 32.80 / 63.05 ms | 每模型并发上限 8 | 514/s |
| Action outbox：terminal commit + completion enqueue | 257 actions | 487.80 / 748.90 us | pending high-water 257；oldest 135,261 us | 1,893/s enqueue |
| Action outbox：claim + reconcile / ack | 257 claims / acks | 1,848.40 / 2,623.50 us claim；398.80 / 642.30 us ack | pending 从 257 排空至 0 | 415/s drain |
| Memory outbox：durable marker enqueue | 257 markers | 383.00 / 614.90 us | pending high-water 257；oldest 103,019 us | 2,487/s enqueue |
| Memory outbox：depth-257 pending scan / conditional ack | 各 257 次 | 225.30 / 372.60 us scan；395.90 / 637.90 us ack | 257 条恢复查询 | 2,442/s clear |

这是单机单次运行结果，不设性能阈值。LLM 的 2ms mock 使用 Tokio timer；Windows timer 粒度、permit 排队和调度共同进入观测值，32.80/63.05ms 不代表真实 provider latency。Action 的 claim latency 包含按 terminal action history 执行的 reconcile；Memory profile 只到 durable marker/list/ack，不含内存 worker inference、provider 调用或 fact persistence。UI 数值只覆盖 reducer/store/selector dispatch，不含浏览器 DOM 与 paint。Profile fixture 代表性有限，需要在相同入口和 profile 下复跑后才适合比较变化。

| 链路 | 复跑入口 | 当前可观察 | 尚缺且未测量 |
|---|---|---|---|
| SessionActor mailbox | `cargo test --locked -p haven-agent --lib actor_mailbox_latency_profile_by_prefilled_depth -- --ignored --nocapture --test-threads=1` | actor 的 `snapshot` 入队到 oneshot 返回分布；预填深度 0/32/64/128、容量 128 的队列高水位与串行 roundtrip 吞吐；另有 provider-await/fairness 行为测试 | 不包含生产负载下的多 producer 混合命令分布；此入口用 debug test profile，`snapshot` 成本代表轻量命令往返，不是所有命令的统一成本 |
| Session event replay | 上述 `active_replay_boundary_benchmark_1k_10k_100k` 命令 | 每个 fixture 的 full 与 compact-summary active suffix 读取 p50/p95；生产 `read_active` 仍有 `scan_ms` debug 字段 | 内存 fixture 不覆盖持久磁盘、冷缓存、回滚 fallback、完整 session startup/resume 时间 |
| UI reducer broadcast | `corepack pnpm --dir ui run profile:session-reducer`；`corepack pnpm --dir ui run test:run -- src/lib/sessionReducer.test.ts src/lib/sessionSelectorStore.test.ts` | 1k/10k transcript 的 `agent/chunks` dispatch p50/p95、吞吐、0/16/64 active selector subscriber 数及每次实际通知量；行为测试继续检查 equality/cleanup | 单一代表性 chunk 更新；不估算浏览器 paint、DOM、heap allocation 或完整组件树耗时 |
| LLM request | `cargo test --release --locked -p haven-llm llm_request_latency_and_concurrency_profile_with_mock_provider -- --ignored --nocapture --test-threads=1`；固定 ReAct 场景后从 Settings 导出性能快照 | mock-immediate router completion 分布/吞吐；固定 2ms mock service 在 8 permits 下的并发排队分布/吞吐；现有 ReAct `request_context`、`first_token`、`llm_stream` histogram | mock 不含真实 DNS/connect/TLS/provider、流解码或 provider 限流；请求 profile 的固定 mock service wait 不应当作网络或生产 latency |
| Action completion outbox | `cargo test --release --locked -p haven-agent --test core_pipeline_performance action_completion_outbox_latency_depth_and_throughput_profile -- --ignored --nocapture --test-threads=1`；`cargo test --locked -p haven-tools action_service::tests` | 临时文件 SQLite 上的 terminal+outbox commit、claim/reconcile、ack 分布，pending 高水位、最老等待年龄及 enqueue/drain throughput；行为测试保留 CAS、恢复、重试和 late-attach 覆盖 | 样本为单个固定批次，claim/reconcile 成本包含当前 action history 全量 reconcile；没有模拟 transcript projection 消费者 |
| Memory fact extraction outbox | `cargo test --release --locked -p haven-agent --test core_pipeline_performance memory_fact_outbox_latency_depth_and_throughput_profile -- --ignored --nocapture --test-threads=1`；`cargo test --locked -p haven-agent memory_worker` | 临时文件 SQLite typed store 上 durable marker enqueue、固定深度 pending scan、conditional ack 分布及队列年龄/吞吐；worker 行为测试保留重试、coalescing 和 cancellation 覆盖 | 本地 store profile 不运行事实抽取或 provider；不能推断 enqueue→LLM→facts persist 的端到端服务时间 |

行为测试的总运行时不作为基线。ReAct phase metrics 是固定桶估算；provider/network 等待不等于 crate-local 成本。读取/导出的计时结果必须继续附场景、样本数、输入规模、profile、单位和边界；没有合适夹具时应报告缺口，不推断延迟。

本 ADR 的 100k 内存 SQLite replay 基线不定义事件保留期限或存储容量。当前 session 事件保留与尚未覆盖的磁盘增长、容量告警和低磁盘 durable append 行为见 ADR 0389。

## 影响与验证

- 生产 actor、存储、outbox 与 reducer 的调度、顺序、取消/重试和 wire/storage 契约不变。
- 无新增依赖、生产指标或 runtime policy；profile 只存在于忽略执行的 Rust 测试和专用 UI 脚本，不进入默认运行路径。
- 验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`；单独复跑命令会打印 replay baseline 行。

## 回滚

删除本 ADR 与手动 profile harness 即可；无数据库、配置、IPC 或用户数据重置要求。
