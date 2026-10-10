# ADR 0874：按 root 角色命名生成媒体路径

## 状态

Accepted — 2026-10-10

## 背景

`default_generated_media_dir()` 返回 `ConfigLoader::data_dir()/media/generated`，调用方把它用作 generated-media 的写入、managed asset 注册、附件读取校验、文件交接和 App 清理边界。多个调用点已把该值命名为 `generated_root`、`capture_root` 或 `generated_media_root`；Common API 的 `dir` 没有表达这一路径承担的 root 角色，也与 `default_runtime_temp_root()` 的命名不一致。

## 决定

- 将 Common API `default_generated_media_dir()` 全面重命名为 `default_generated_media_root()`，删除旧入口，不保留兼容 alias。
- 保留现有路径 `ConfigLoader::data_dir()/media/generated`、创建时机和各领域生命周期策略。
- Common 只提供该稳定 root 路径，不接管 App 的文件遍历/删除、Tools 的资产注册/租约或 Memory 的附件读取安全策略。
- 历史 ADR 中的旧符号名保留为历史记录；当前源码和规范文档统一使用新名称。

## 影响与兼容性

本次只改变 Rust 源码 API 和当前命名文档，不改变文件位置、配置字段、IPC、数据库或文件格式。项目处于不兼容重构阶段，不提供旧函数名转发；无需数据库或配置重置。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`，以及新 ADR 文件的 Prettier 检查。
测试套件未运行。

## 回滚

如某一调用方需要不同的媒体存储边界，应显式传入其领域 root；不恢复 `default_generated_media_dir()` 兼容包装。当前无持久数据需要恢复。
