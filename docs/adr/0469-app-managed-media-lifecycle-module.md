# ADR 0469：隔离 App 托管媒体文件生命周期

## 状态

已采纳；实现进行中（2026-10-05）。

## 背景

`commands/recording.rs` 同时处理录音/Tauri 事件状态机与一整段托管文件生命周期：附件配额、staging 写入和原子提交、资产登记，以及 uploads/generated-media 两个根目录的清理。文件操作、`UPLOAD_WRITE_LOCK`、`SessionStore` 持久附件引用与 `ManagedAssetRegistry` lease 必须一起审查；它们目前夹在录音命令及测试中。

现有行为由 ADR 0403、0118 和 0454 约束：持久消息路径是清理引用权威；pending/session lease 与 detached generated-media TTL 保护尚无持久消息引用的资产；staging 有独立 24 小时 TTL。`app_state.rs` 负责启动/每日调度，`commands/session.rs` 在成功删除历史后触发清理。清理引用查询失败必须跳过两个媒体根目录，且仍可继续独立的 staging 清理。

审计同时确认两项直接属于文件生命周期的漂移：剪贴板复制文件采用 `file-{uuid32}-{filename}`，但 generated-media 清理只接受 `file-{uuid32}.{extension}`；staging cleaner 未先拒绝 uploads 根目录本身的符号链接/重解析点。两个媒体根目录的扫描目前按顺序执行并短路，第一根扫描失败会跳过第二根。

另有一个不同边界的并发风险：`haven-tools` 的媒体 producer 在文件落地后才注册 lease，不共享 App 的 `UPLOAD_WRITE_LOCK`，因此文件写入到 registry 登记之间存在窄窗口。本 ADR 不声称解决跨 crate producer 与 GC 的原子性；该问题保留为独立候选，不通过移动代码或暗增共享锁掩盖。

## 决定

1. 新建 `commands::managed_media` 私有模块，统一持有 App 上传落盘、容量扫描、文件名与路径校验、staging 生命周期、两个媒体根目录清理以及 `UPLOAD_WRITE_LOCK`。`recording.rs` 保留录音/Tauri 命令和 transcript 编排，并调用该模块完成附件落盘。
2. 模块通过现有 `SessionStore` typed port 读取 durable attachment references；引用读取失败在任何媒体删除前返回错误。SessionStore 仍拥有查询实现；AppState 仍拥有 retention/startup/daily 调度；session 命令仍拥有 delete/clear 生命周期。`ManagedAssetRegistry` 继续拥有进程内注册、lease、pending lease 和 TTL，不迁移其实现或另建状态源。
3. 同一 `UPLOAD_WRITE_LOCK` 覆盖 quota 读取、staging 写入、目录原子提交、pending/session lease 登记，以及两个媒体根目录和 staging 的清理。不得在新模块或调用方另造锁。上传后 `process_transcript` 对 pending/session lease 的绑定、转移和释放顺序保持不变。
4. 将 generated-media 名称校验扩展为同时识别已存在的 `file-{uuid32}-{filename}` 剪贴板产物；仍要求固定 `file-`、32 位 hex ID 和直接子文件项。uploads 根、generated-media 根、staging 根及待处理目录项都拒绝符号链接/重解析点。
5. 两个媒体根独立尝试清理：一根扫描失败不阻止另一根继续；如任一根失败，完成两次尝试后返回可观测错误。单个目录项删除失败继续处理其它项。引用集合缺失/读取失败仍短路两个根目录，不改变 fail-closed 语义。staging 清理仍独立于 history retention 与引用查询。
6. 本切片不改变 Tauri 命令、IPC、数据库/config schema、资产 ID、清理调度频率或用户数据格式；不移动录音状态机、session deletion、Tools 生成器或 registry owner。

## 替代方案

- 仅把方法搬到另一个文件但让清理调用方继续分别读取引用或各自持有锁：拒绝。它会留下多个清理入口/锁契约，不能让上传与两根目录清理作为一个生命周期审查。
- 把 startup/daily scheduler 或 `ManagedAssetRegistry` 搬入新模块：拒绝。调度仍属于 AppState 生命周期，lease 与 runtime TTL 仍属于 Tools registry。
- 将 `UPLOAD_WRITE_LOCK` 暗中扩展为跨 crate 的所有媒体 producer 锁：暂缓。需要独立设计注册前 reservation/lease、取消清理与并发测试，不以模块提取顺带改动隐藏这个契约。

## 影响与验证

模块提取无持久数据变化，无需数据库重置。回归验收包括：附件 quota 串行化、部分 root 失败时另一根仍被尝试、staging 上传失败回滚、引用查询失败时两根保持原状、strict generated naming、root/item reparse rejection、 durable reference 与 active/pending lease/TTL 保护，以及上传与清理并发时不删除尚处于 lease handoff 的文件。Tauri IPC 与现有上传 DTO 保持不变。

适用门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-app-binary`、`cargo check --locked -p haven-app-binary`、`cargo clippy --locked -p haven-app-binary -- -D warnings`、`git diff --check`。重解析点拒绝还须在 Windows 文件系统上实际验证；若测试环境不能创建 reparse point，ADR 记录该限制并保留 Windows 验收项。后续另行审查 Tools producer → registry lease 的登记窗口。

## 回滚

将 `commands::managed_media` 实现移回 `recording.rs` 并恢复原调用即可；该决定不改持久数据、IPC 或用户设置。新增加的剪贴板名称识别与 staging 根拒绝是生命周期 bug 修复，回滚代码时应单独保留其回归修复，不应随模块移动一并撤销。
