# ADR 0596：命名 managed-media cleanup 计数

## 状态

已采纳并实施。

## 背景

App 清理用户上传目录与生成媒体目录时，组合 helper 返回 `(uploads, generated)`。两项 `usize` 的含义依赖固定位置：第一项是删除的上传批次目录数，第二项是删除的生成媒体文件数。调用者必须在日志和条件中重复记住这层约定。

## 决定

1. 增加 `ManagedMediaCleanupCounts`，字段为 `removed_upload_batches` 与 `removed_generated_media_files`。
2. managed-media 两根目录的组合清理路径统一返回该具名结果；单根目录清理仍返回单个 `usize`。
3. 调用端按字段判断和记录计数。

## 替代方案

- 返回 `(usize, usize)` 并只补注释：拒绝，调用处仍必须从 tuple 位置理解实体类别。
- 将底层单目录 helper 也包装成相同结构：拒绝，单个计数没有第二个同类字段需要消歧。

## 影响与验证

- 仅调整 App 内部清理结果的 Rust 类型，不改变数据库、IPC、目录顺序、失败处理或清理行为。
- 命名路线图仍保持 Active，其他 Rust crate、UI、IPC/event 与配置持久名称继续逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-app-binary`、`cargo clippy --locked -p haven-app-binary -- -D warnings`、`cargo test --locked -p haven-app-binary`、ADR 索引及 staged diff 检查。

## 回滚

恢复为 `(uploads, generated)` 并还原解构调用点；无持久化迁移。
