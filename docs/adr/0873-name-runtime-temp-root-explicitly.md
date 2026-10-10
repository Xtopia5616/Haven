# ADR 0873：明确命名应用运行时临时根

## 状态

Accepted — 2026-10-10

## 背景

`haven_common::default_work_dir()` 返回系统 Temp 下的 `%TEMP%/haven`，并非 `SkillsExecConfig.work_dir`：后者是可配置的 Skill 脚本执行 cwd，默认位于前者的 `skills_work` 子目录。Common helper 还被用作工具默认 cwd，以及上传、工具输出日志、venv 安装和 Agent prompt 运行快照的共享临时根。泛名 `default_work_dir` 容易让调用者把应用级临时根与 Skill 脚本工作目录当成同一 owner。

## 决定

- 将 Common helper `default_work_dir()` 全面重命名为 `default_runtime_temp_root()`，删除旧入口，不保留兼容 alias。
- helper 仍返回并按需创建系统 Temp 下的 `haven` 目录；不改变任何子路径、shell cwd、文件生命周期或错误行为。
- `SkillsExecConfig.work_dir` 继续表示 Skill 脚本的可配置执行 cwd，默认值仍为 `default_runtime_temp_root()/skills_work`。
- 上传目录、generated media、工具日志和脚本执行目录继续由各自调用者选择子路径。Common 不接管 managed-media root policy；其边界见 ADR 0863。
- `default_runtime_temp_root` 可用于运行命令和暂存文件，但它不是 durable data root；持久配置和数据库仍由 `ConfigLoader::data_dir()` 管理。

## 影响与兼容性

本次只改变 Rust 源码 API 和当前命名文档，不改变系统 Temp 下的路径、配置字段、IPC、数据库或文件格式。项目处于不兼容重构阶段，不提供旧函数名转发；历史 ADR 保留当时使用的名字以便检索。本次无需数据库或配置重置。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`，以及新 ADR 文件的 Prettier 检查。
测试套件未运行。

## 回滚

若后续证明调用方需要不同的临时根，应为新 root 定义明确 owner 与名称；不恢复含义不清的 `default_work_dir()` 兼容包装。当前无持久数据需要恢复。
