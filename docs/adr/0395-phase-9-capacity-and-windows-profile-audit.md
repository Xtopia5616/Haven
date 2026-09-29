# ADR 0395：阶段 9 容量验证与 Windows profile 验收状态

## 状态

已采纳（固定长度容量 profile、混合事件 profile、SQLite `SQLITE_FULL` 与 deferred `COMMIT` 故障恢复于 2026-09-29 完成；`haven-memory` 全部 374 个测试通过。当前合并工作树的全量门禁尚未通过：Agent 错误提示测试被 `haven-tools` 中缺少 `McpServerConfig.env_refs` 的构造阻塞，UI 类型检查被 `SettingsView` 缺少 `history_retention_days` 阻塞。物理盘 ENOSPC、真实 Windows GUI/安装 profile 验收仍开放。容量与保留决策见 ADR 0404。

## 范围

记录阶段 9 的 session event 文件容量、retention/WAL 行为、低容量 durable append 失败与恢复，以及本轮 Windows 桌面验收的环境限制。关联 ADR 0360、0361、0389；实现测试位于 `crates/memory/src/repositories/session_event_capacity_tests.rs`。

## 磁盘容量测量

使用 Windows 文件系统上的临时 SQLite WAL 数据库；每组先创建当前 schema v31 和一个 session，随后通过生产 `SessionStore::append_batch` 写入 1,000 条 transcript event。payload 是重复 ASCII 文本组成的固定长度有效 JSON，页面大小为 4,096 字节。表中 `db_after_append - baseline_db` 是 1,000 条事件增加的主库文件大小；`wal_peak` 是每批 append 后观测到的最大 WAL 文件长度，不是事务内部每条写入间的绝对峰值。

| 每条 event payload | 初始主库 | 写入后主库 | 增长/1,000 条 | 批次边界观测 WAL 峰值 | retention 删除并 checkpoint 后主库 | freelist 页 | VACUUM + checkpoint 后主库 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 512 B | 266,240 B | 1,056,768 B | 790,528 B | 1,112,432 B | 1,056,768 B | 193 | 266,240 B |
| 4 KiB | 266,240 B | 4,980,736 B | 4,714,496 B | 4,507,312 B | 4,980,736 B | 1,151 | 266,240 B |
| 16 KiB | 266,240 B | 17,268,736 B | 17,002,496 B | 4,466,112 B | 17,268,736 B | 4,151 | 266,240 B |

三组 retention 后的 `wal_checkpoint(TRUNCATE)` 均清空 WAL。whole-session retention 会通过外键级联删除 session events；checkpoint 后，数据库主文件仍保持原高水位，释放页进入 freelist 并可供后续写入复用。当前 schema 未启用自动 vacuum，应用的定期 cleanup 也不运行 `VACUUM`。手动 `VACUUM` 再 checkpoint 后，本 fixture 的主库回到 266,240 B。该回收需要额外磁盘空间和完整重写，不能安全地默认为启动清理步骤。

这些是合成 payload 的存储放大测量，不代表 Haven 用户的事件大小分布、单 session 长期上限、并发写 WAL 高峰或最小磁盘需求。结果显示高水位随最大历史数据增长；删除 session 释放可复用页，但不会自动把文件缩回基线。

## 合成混合事件 profile

`session_event_representative_mixture_capacity_profile` 在 Windows 文件型 SQLite 上通过生产 `SessionStore::append_batch` 写入 1,000 条按 TranscriptRecord 字段形状构造的合成事件。观测如下：

| 指标 | 观测值 |
|---|---:|
| payload 总量（JSON UTF-8 bytes） | 2,223,686 B |
| 单条 payload min / median / p95 / max | 609 / 1,754 / 3,887 / 8,922 B |
| page size / append 后 page count | 4,096 B / 754 |
| append 前主库 / append 后主库 | 266,240 B / 3,088,384 B |
| 主库增长 / 批次边界 WAL 峰值 | 2,822,144 B / 3,155,952 B |
| retention 删除并 checkpoint 后主库 / freelist | 3,088,384 B / 689 页 |

样本比例与字段长度见上文；它不是用户数据抽样。profile 在 retention 后确认 WAL 可截断为 0，但主库保留高水位、空闲页进入 freelist。该数据只用于比较指定合成场景，不推导容量阈值、最低磁盘需求或长期增长速度。

## SQLite 容量耗尽与重试

正常回归测试 `sqlite_full_transcript_commit_rolls_back_and_can_be_retried` 用文件型数据库和单连接测试 fixture，将 `PRAGMA max_page_count` 降至当前页数，再提交 1 MiB transcript event 及 assistant message projection。写入返回 SQLite full 错误；事件、消息 projection 与 live broadcast 均不存在。移除页数上限后重试同一提交成功，sequence 为 1，event、projection 和 broadcast 一致。

此测试证明 SessionStore 把 `SQLITE_FULL` 当作失败返回，并保持 transcript event、projection 与发布的事务边界；恢复可用空间后可以重试而不留下 sequence 空洞。SQLite 页数上限不是 Windows 物理盘 ENOSPC 故障，不验证实际操作系统返回码或完整桌面恢复流程。

额外的 deferred foreign key 故障让 event/projection 写入成功而 `COMMIT` 本身失败。SessionStore 对事务体失败和 `COMMIT` 失败都尝试回滚；失败提交不广播，连接回到 autocommit 后，同一 transcript 可重新提交为 sequence 1。该注入覆盖的是 SQLite 提交阶段失败处理，不是磁盘耗尽。

容量用户反馈现对 SQLite `SQLITE_FULL` 和较宽泛的 SQLite disk I/O failure 分类：前者提示释放数据库所在磁盘空间，后者要求检查空间和磁盘可用性；两者均说明删除 session 不保证缩小主数据库文件。disk I/O failure 不能判定为磁盘已满。该反馈可以从当前的 `session:error` 实时送达，不依赖再写一条 durable event；物理盘 ENOSPC 的 Windows 展示和恢复路径仍需 GUI profile 验收。

复跑命令：

```text
cargo test --locked -p haven-memory sqlite_full_transcript_commit_rolls_back_and_can_be_retried -- --nocapture
cargo test --locked -p haven-memory failed_transcript_commit_rolls_back_and_can_be_retried -- --nocapture
cargo test --locked -p haven-memory sqlite_storage_failures_keep_full_and_io_errors_distinct -- --nocapture
cargo test --locked -p haven-agent full_database_error_gives_recovery_guidance_without_database_details -- --nocapture
cargo test --locked -p haven-memory session_event_disk_capacity_profile -- --ignored --nocapture --test-threads=1
cargo test --locked -p haven-memory session_event_representative_mixture_capacity_profile -- --ignored --nocapture --test-threads=1
```

固定长度 profile 写入三个临时数据库和约 20 MiB 的合成 event 数据；混合 profile 写入另一个数据库并追加约 2.2 MiB payload。两个测试结束后都会删除各自专属 `%TEMP%` 目录；日常测试默认忽略这些 profile。

## 当前容量策略

1. Session event 在 session 保留期间仍完整 append-only；compaction 与 rollback 不裁剪历史。
2. 保留现有 whole-session age retention：默认 90 天，设置为 `0` 时禁用；用户也可显式删除 session。它不是字节上限。
3. 当前测试版本不新增单 session/数据库字节上限、事件归档、固定容量告警阈值或容量百分比。现有合成样本不代表用户分布，无法选出合理阈值；静默裁剪会破坏恢复/回滚权威，归档则需要新的恢复契约。写入失败时的原因分类和恢复提示见 ADR 0404。
4. 不在 retention cleanup 中自动执行 `VACUUM`；SQLite freelist 可复用空间，低磁盘时全库重写可能需要额外空间。
5. `session_event_representative_mixture_capacity_profile` 以生产 TranscriptRecord 字段形状构造 1,000 条合成混合事件，补充 payload 分位数和文件增长观测。其比例与文本长度是工程场景，不是采样的真实用户事件分布；不据此确定最低磁盘需求。每 session 和数据库总字节容量上限均不承诺。
6. disposable Windows profile/VM 仍须验证物理盘满错误、Windows VFS 返回码、桌面提示与恢复操作；当前容量注入不是该验收的替代。Windows GUI、安装升级/卸载和卸载后的数据保留也继续开放。

混合样本比例固定为 Thought 25%、Reasoning 15%、UserInject 15%、ToolCall 20%、ToolResult 20%、CompactSummary 5%。相应正文长度分别取 512 B、2 KiB、1 KiB、ToolCall 参数 1 KiB 加 reasoning 512 B、ToolResult canonical/history observation 1/2 KiB 加 tool input 512 B，以及 summary 8 KiB。payload 由字段化 JSON shape 序列化，压测数据不含用户文本或附件。

## Windows profile/GUI 验收

以下首启和重置 smoke 是 schema v30 时点的历史记录，不代表当前 schema v31 的 Windows profile 验收。

在当时工作树中，曾于当前 Windows 用户下配置独立数据根（不是单独 Windows 账户或 VM）：`target/test-data/phase9-windows-profile-current-20260929`。用进程级 `APPDATA`、`LOCALAPPDATA`、`TEMP`、`TMP` 重定向路径；该目录内的 `README.md` 保存了可复跑启动命令。项目锁定的 Rust 1.98.0、Node 24.20.0、pnpm 11.24.0 均用于当时的 release 构建，命令为 `cargo tauri build --no-bundle --ci`。

当时构建的 release app 在全新 profile 首次启动后保持响应，窗口标题为 Haven。默认 `config.toml`、数据库、WAL/SHM 与日志均写入隔离数据根；日志记录 `AppState ready` 和 `Haven Tauri app initialized`。数据库 `user_version=30`、23 张用户表，`integrity_check=ok`、外键违规为 0。默认 profile 未配置模型 endpoint。该 v30 检查证明进程可创建首启数据并完成 bootstrap，但没有验证画面内容或交互流程。

当时 Computer Use 的 `getState()` 因 Codex auth token unavailable 无法列出/操作窗口；窗口留在隔离 profile，供后续手动反馈。该验收期间账户不是管理员，Windows Sandbox executable 不存在，查询 Windows optional feature 也要求提权。机器已有 MSI/NSIS bundle 时间戳为 2026-09-14，早于 schema v30；主机没有 `makensis.exe`，winget 当前用户范围安装未找到适用安装器，因此没有构建或运行新 installer。真实 `%APPDATA%\Haven` 未被该次应用访问或改动。

### 先前 v30 工作树的自动化门禁记录

以下全量门禁是在 schema v31 与 credential/authorization 合并改动之前的历史结果，不能作为当前合并工作树的通过结论。

- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo test --workspace --locked` 在最新工作树通过；包含 `haven-llm` 471 tests、`haven-memory` 369 个测试用例（1 ignored）、`haven-tools` 806 passed / 2 ignored、MCP integration 7 passed，以及其他 workspace crate suites。早先运行时遇到的 `RequestProfileClient::health_check` 编译错误在本次最终全量运行前已不再出现。
- UI `check` 通过，0 errors / 0 warnings；`test:run` 通过，874 tests；release UI build 通过。
- `scripts/check-ipc-contracts.ps1` 与 `scripts/check-ipc-events.ps1` 通过，覆盖 71 个 commands 和 40 个 event channels。

### 当前合并工作树的容量切片验证

- `cargo test --locked -p haven-memory` 通过：374 passed、2 ignored；包含 `SQLITE_FULL` 原子失败/重试、deferred `COMMIT` 回滚、SQLite full/I/O 分类与年龄 retention 级联测试。
- 固定长度 profile 与合成混合 profile 均通过。混合 profile 的观测值见上表；测试结束后清理其专属临时目录。
- 当前分支全量 Rust 门禁尚未完成。`cargo test --locked -p haven-agent full_database_error_gives_recovery_guidance_without_database_details -- --nocapture` 在编译 `haven-tools` 时被 `crates/tools/src/builtin/admin_services.rs` 的 `McpServerConfig` 初始化缺少 `env_refs` 阻塞。
- UI `test:run` 的 874 项测试和 `pnpm run build` 通过；`pnpm run check` 仍因 `ui/src/lib/views/SettingsView.svelte` 传入的 `SessionConfig` 缺少 `history_retention_days` 而失败。全量 workspace fmt/check/clippy/test 尚无最终通过结果。

剩余发布验收包括：在可操作的 Windows profile/VM 中目视检查首启界面和 Settings、会话、确认工具、媒体、任务、恢复/回滚交互；取得当前 NSIS/MSI bundle 后覆盖真实安装、升级、卸载及卸载后的数据保留。物理盘 ENOSPC 和桌面错误/恢复文案也仍未验证。当前隔离 APPDATA smoke 不能代替这些检查。应使用隔离的模型/媒体 fixture，不把真实凭据或用户数据带入该 profile；记录 OS、bundle 版本、临时数据根路径、schema 版本和每项通过/失败。

同一 v30 时点还在 `target/test-data/phase9-reset-profile-20260929` 顺序验证了数据库重置：人为设置 `user_version=29` 后，启动日志明确拒绝该库并提示删除数据库；按发布文档仅删除 `haven.db`、`haven.db-wal`、`haven.db-shm` 后重启，配置文件 SHA-256 保持不变，新库为 v30 且 integrity check 通过。测试发现 `tauri_plugin_single_instance` 按 Windows 用户限制同时运行一个 Haven 实例；profile smoke 必须先完全退出既有实例，再启动另一组 APPDATA。

因此阶段 9 仍开放；本 ADR 不把 SQLite fault injection 冒充为物理盘满验收，也不把 bootstrap smoke 或单元测试冒充为 UI 安装、升级和卸载验收。
