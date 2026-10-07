# ADR 0657：明确 Agent chunk event consumer 任务句柄

## 背景

`EventDispatcher::spawn_chunk_consumer_raw` 实际启动的是 Agent thought/reasoning chunk batcher 的事件 consumer；`raw` 没有对应的另一种 API。它返回 `Option<JoinHandle<()>>`，但实现始终创建任务并返回 `Some`。消费者 `StreamForwarder` 也只在 `Some` 时等待，因此 optional 没有表达有效状态，只让 task 的 owner 和必需生命周期变得含糊。泛名 `ConsumerHandle` 也没有说明是哪条队列的 consumer。实现还额外 spawn 一层 task 等待 batcher task，并丢弃内层 join error，没有增加独立生命周期。

## 决定

- 方法改名为 `spawn_chunk_event_consumer`，明确创建 ordered chunk event consumer。
- 删除 `ConsumerHandle` alias，返回 Tokio `JoinHandle<()>` 本身。
- `StreamForwarder` 直接拥有 batcher 本身的 `chunk_consumer_task`；移除只等待内层任务的 wrapper task，flush 必定等待真实 batcher 并观察其 join error。
- 队列容量、聚合行为、事件顺序、关闭与错误传播不变。
- 本 crate 内没有保留旧方法或类型 alias。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

恢复旧结构需重新引入 optional handle 与 wrapper task。本次只改进程内 API，无数据迁移。
