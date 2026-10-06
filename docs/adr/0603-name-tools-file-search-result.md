# ADR 0603：命名 Tools file search 结果

## 状态

已采纳并实施。

## 背景

Tools 文件搜索支持 filename 与 content 两种模式；`search_filenames_parallel`、`search_content_parallel` 和 `search_files` 均返回 `(Vec<Value>, Option<TruncationReason>)`。上层把第一项作为匹配项列表，第二项作为搜索是否因 max-results 或扫描上限截断的原因，并据此构造 JSON `results`、`has_more`、`truncated` 与提示。搜索数据和覆盖完整性有不同稳定语义。

## 决定

1. 搜索函数统一返回私有 `FileSearchResult { results, truncation_reason }`。
2. filename/content 两种路径继续共用同一结果字段；顶层按具名字段构造原 JSON 输出。
3. 截断优先级、扫描限制、取消处理、并发遍历和排序行为保持不变。

## 替代方案

- 保留 tuple，仅修改局部变量名：拒绝，三层搜索函数与工具入口仍靠位置配对匹配项和截断状态。
- 将截断状态塞入每条匹配记录：拒绝，截断是整次搜索的属性，不属于单条文件匹配。
- 为 filename 与 content 定义两种结果类型：拒绝，两种搜索返回完全相同的匹配及截断语义，没有独立 owner 或字段约束。

## 影响与验证

- 仅变更 Tools 内部搜索实现类型；不改 JSON/wire shape、搜索模式、结果排序、限额或用户提示。
- 命名路线图 §5.7 继续保持 Active。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo test --locked -p haven-tools`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(Vec<Value>, Option<TruncationReason>)` 并还原三个搜索函数和顶层工具的 tuple 使用；无持久化迁移。
