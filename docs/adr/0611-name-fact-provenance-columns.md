# ADR 0611：命名 Fact provenance column projection

## 状态

已采纳并实施。

## 背景

`FactGraph::provenance_cols_from_source_ref` 将一个规范化来源映射到三列持久值：引用可能落在 `provenance_item_id` 或 `provenance_record_id`，并可带 `provenance_snippet`。原返回三元素 tuple，insert 与 reinforcement update 都按位置把值绑定到 SQL 参数。

## 决定

1. 用私有 `FactProvenanceColumns` 表达三个持久列值。
2. 按 `provenance_item_id`、`provenance_record_id` 与 `provenance_snippet` 字段绑定 INSERT/UPDATE 参数。
3. 对无引用或缺少消息 ID 的来源，未设置的列保持 `None`。

## 替代方案

- 保留 tuple 并把局部变量命名：拒绝，SQL 参数位置仍需与返回位置同步记忆，修改列顺序时容易错绑。
- 把 item id 和 record id 合并成一个字段：拒绝，两者对应不同 FK/引用约束与数据库列；同一 message id 只会按实际存储 owner 写入其中一个。

## 影响与验证

- 仅改变 Memory fact graph 的内部结果类型；来源脱敏、引用归类、数据库列和值保持不变。
- 命名路线图 §5.7 继续保持 Active；其他 repository 与全仓边界命名仍待逐项复核。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-memory`、`cargo clippy --locked -p haven-memory -- -D warnings`、`cargo test --locked -p haven-memory`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(Option<String>, Option<String>, Option<String>)` 与 SQL 参数 tuple；持久列和数据不需要迁移。
