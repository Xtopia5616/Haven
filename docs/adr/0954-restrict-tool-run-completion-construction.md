# 0954：限制 ToolRun completion/fire DTO 的下游构造

## 状态

已接受并实现（2026-10-11）。

## 背景

`BackgroundToolRunCompletion`、`ScheduledToolRunResultCompletion` 与 `ScheduledToolRunFired` 都是 Tools 在 ToolRun 终态或 scheduled fire transition 中产生的跨 crate contract。实际构造点位于 `haven-tools` 内部；Agent 只消费 completion 并向所属 session 投影，scheduled-fire consumer 只读取事件并执行 owner port，App 仅转接 receiver。下游没有直接构造这些值的生产代码或测试 fixture。

三个 DTO 的公开字段允许下游 struct literal 自行伪造 ToolRun lifecycle 输入，与 Tools 对状态转换、发布和恢复的所有权不符。

## 决定

- 将三个 producer-owned DTO 标为 `non_exhaustive`，保留消费者读取字段与接收 contract 的能力，同时阻止下游 struct literal 构造。
- 不增加 `test-support` constructor：当前没有跨 crate 测试需要自行创建这些值；Tools 内部单测仍可覆盖其生产路径。
- 保持字段、enum variant、传输内容、completion/recovery 行为和 IPC 映射不变。

## 影响

Tools 继续唯一负责 completion/fire DTO 的构造与发布。Agent、App 以及其他下游保留消费能力，但无法通过公开字段组合新的值。IPC、持久化、session transcript 和安全语义均不变，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --cached --check`

## 回滚

若出现真实的跨 crate producer，应先确认其职责归属；若确由 Tools 管理生命周期，新增具名 owner 操作。如果未来确有下游 projection fixture 需要构造，则通过非默认 `test-support` feature 提供测试入口，不恢复默认生产构造能力。
