# ADR 0594：为 builtin tool resume selection 命名字段

## 状态

已采纳并实施。

## 背景

Agent resume 从持久 ToolCall 的 JSON 输入恢复 builtin lazy-load 选择。`builtin_selection` 返回 `(operations, roots)`，消费者必须通过 tuple 位置区分 operation 子集和 root 集合；缺失或类型不正确的数组表示没有可安全恢复的选择，不能扩大为 load-all。

## 决定

1. 解码入口改为 `decode_builtin_tool_selection`，返回 `BuiltinToolSelection { operations, roots }`。
2. 两个字段仍分别按原 JSON key 读取，仅接受字符串数组成员，trim 并去重；缺失或非数组字段仍为 `None`。
3. Resume driver 仍只在至少一个列表非空时调用 overlay loader；空、缺失和畸形输入不会新增工具。

## 替代方案

- 只给 tuple alias：拒绝，消费者依旧依赖索引位置。
- 将 operations 和 roots 合并成一个 names 列表：拒绝，它们是两个不同 loader 输入范围，恢复行为不能混淆。

## 影响与验证

- 改动限于 Agent resume 的私有持久输入投影，不改变持久 JSON、event schema、工具注册顺序或恢复行为。
- 更新命名路线图；无需数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent`（607 passed / 1 ignored；2 manual performance tests ignored）、ADR 索引与 `git diff --check`。

## 回滚

恢复 tuple decoder 与其调用方位置解构；持久事件和 JSON 无需迁移。
