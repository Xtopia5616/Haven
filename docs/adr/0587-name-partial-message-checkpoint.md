# ADR 0587：为 Memory 流式草稿命名 checkpoint 结果

## 状态

已采纳并实施。

## 背景

Memory 的 `partial_messages` 表暂存正在生成、尚未进入 canonical transcript 的 assistant 文本。`Database::get_partial_message` 与 `take_partial_message` 将 `content` 和 `updated_at` 作为 `(String, String)` 返回给 Agent；调用方只能依赖字段顺序，promotion 也需要用时间戳判断草稿是否已被较新的 transcript 消息覆盖。

## 决定

1. Memory 定义并导出 `PartialMessageCheckpoint { content, updated_at }`。
2. 读取与原子 take 两个入口均返回 `Option<PartialMessageCheckpoint>`；Agent 和 promotion 按字段名消费内容与检查点时间。
3. 该类型表示 canonical transcript 外的流式草稿 checkpoint，不代表已提交的 `Message`。

## 替代方案

- 保留 `(String, String)` 并只在文档注明顺序：拒绝，跨 crate 调用仍依赖位置约定。
- 复用 `Message`：拒绝，checkpoint 尚未进入 transcript，缺少真实消息的身份、角色和提交时间语义。

## 影响与验证

- 更新 Memory partial-message 查询、take、promotion、测试，以及 Agent checkpoint 消费点。
- 数据库表、SQL 列名、写入顺序、原子删除、promotion 时间边界和 UI 行为不变；无 schema/config/IPC 变化，无需数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、ADR 索引与 `git diff --check`。

## 回滚

恢复两个 Memory 方法的 `(String, String)` 返回值，并还原 Agent/promotion 的位置读取；表结构和存量数据不需要变化。
