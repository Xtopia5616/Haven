# ADR 1173：统一 managed upload 根路径

## 状态

已采纳并实施。

## 背景

App 上传写入与清理、Memory 附件预览读取和 Tools 资产登记都需要同一个受信 uploads 根。各层曾分别用 `default_runtime_temp_root().join("uploads")` 拼接它，路径一旦调整就可能令写入、授权/登记与读取路径失配。

## 决策

- `haven_common::config::default_uploads_root()` 是 managed uploads 根的唯一路径 owner，返回 runtime Temp 下的 `uploads` 子目录，但不主动创建目录。
- App、Memory 与 Tools 的生产代码通过该 helper 获取 uploads 根。Managed uploads 是由 session durable references、asset identity 和 lease 控制生命周期的附件存储，不属于进程 scratch；只有实际写入方创建目录。
- `default_runtime_temp_root()` 继续服务 Shell 默认目录、Skill 默认 work directory 等各自用途；Temp 根下不同子目录必须遵守各自的 artifact owner 和清理规则。

## 替代方案

- 在每个 crate 中重复拼接子目录：拒绝，会让 managed media 路径在层间漂移。
- 让通用 Temp root 一次性创建并统一清理所有子目录：拒绝，uploads、日志与短期 staging 的生命周期不同。

## 影响与迁移

根路径仍为当前用户 Temp 下的 `haven/uploads`，文件布局、session/asset 引用、权限和清理语义不变。不修改配置、数据库 schema、IPC 或附件实体 ID，无需数据/配置重置。调用方不再因读取此路径而隐式创建 uploads 目录。

## 验证

- Common 单元测试验证 helper 返回 runtime Temp 下的 uploads 子目录，且不依赖文件系统状态。
- 搜索确认生产代码不再自行拼接 `default_runtime_temp_root().join("uploads")`。
- 执行 workspace Rust tests、严格 Clippy 与格式检查。
