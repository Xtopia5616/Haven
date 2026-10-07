# ADR 0674：合并 Tools 文件读取取消包装器

## 背景

`tools::builtin::file_search` 与文档提取都定义了 `CancellableReader`，分别在每次 `Read::read` 前检查 Tokio `CancellationToken`，取消后返回 I/O 错误。两个实现的职责相同；差异在调用策略：文件搜索限制每次最多读取 64 KiB，并使用 `Other` / `file search cancelled`；文档提取不限制单次读取，并使用 `Interrupted` / `document extraction cancelled`。

## 决定

- 将取消检查实现合并到 Tools crate 内部的 `cancellable_reader::CancellableReader<R>`。
- reader 接受可选 cancellation token，并允许调用者设置单次读取上限与取消时的 I/O 错误 kind/message。
- `file_search` 和文档提取分别保留 `cancellable_search_reader` 与 `cancellable_document_reader`，明确表达调用侧 owner 和策略。
- 不保留两份本地 reader 实现。文件搜索和文档提取现有取消检查、读取上限与错误语义不变。

## 考虑过的方案

- 继续保留重复实现：取消检查的核心行为完全一致，两份实现会独立演进并产生行为漂移。
- 统一错误 kind、消息或读取上限：这会改变调用方当前观察到的取消行为，且不属于消除重复 reader 实现所必需的修改。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools`
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行格式与编译门禁。

## 回滚与重置

恢复 `file_search` 与文档提取中的两个本地 reader，并移除 crate 内部模块即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
