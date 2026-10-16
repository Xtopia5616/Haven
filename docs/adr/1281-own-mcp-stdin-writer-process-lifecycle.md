# ADR 1281：让 MCP stdin writer 拥有 guest process 生命周期

## 状态

已实现并通过平台测试。此切片只调整私有 HCS stdin adapter 的所有权；broker MCP launch、session relay、撤权和 stop confirmation 仍未接通，`WorkspaceWrite` 下 stdio MCP 继续 fail closed。

## 背景

ADR 1278 的有界 writer 借用 `&mut HcsGuestProcessOwner`，既通过该借用查询 process exit，也在 abort 时终止 process。长生命周期 supervisor 还必须同时持有 stdout/stderr reader 与 stdin writer；writer 长期借用 process 会阻止 session owner 把 process、输入适配器和输出适配器组合成一个可持久保存的状态对象。

## 决定

- `HcsGuestProcessInputWriter::new` 按值接收并拥有 `HcsGuestProcessOwner`；writer 自身成为 guest process 的唯一 owner，同时继续拥有 stdin `File` worker 的队列、线程和取消状态。
- `poll_process_exit` 仍经 writer 查询它拥有的 process；`abort` 先关闭输入队列，再请求终止 process，并在有界期限内取消/收束同步 stdin worker。
- 正常 `close_input` 仍排空最多 64 KiB 已接受数据后由 worker 关闭 stdin，向 guest 传递 EOF。单帧 16 KiB 和 `WouldBlock` 背压不变。
- writer 的 drop 在输入未关闭或 worker 失败时沿用有界 abort；worker 超时仍独自持有 `File` 并被 detach，不能从外部关闭其正在使用的句柄。process owner 随 writer 结束时仍执行现有 best-effort 终止/句柄关闭。
- stdout/stderr reader 可先从 process owner 取走两个输出句柄，再把 process owner 移交给 stdin writer；该组合用于后续 session owner。当前未接入生产 HCS launcher，也不改变 service 线程或 MCP 授权行为。

## 替代方案

- 让 session owner 持有对 process 的可变引用：拒绝，会阻止同一 owner 保存独立 stdin 与 stdout/stderr adapters。
- 分别用全局 raw HANDLE 管理 process 和 stdin：拒绝，会绕过现有唯一所有权、终止及关闭逻辑。

## 影响与回滚

这是平台私有 Rust API 变更，没有持久化、配置、IPC 或 UI 变化，无需重置。回滚时恢复 writer 对 process 的借用，并删除新 ADR/路线图记录；尚无 MCP 生产调用点。

## 验证

- 平台 stdin writer tests 覆盖输出 reader 与 process writer 的组合、EOF 排空、16 KiB chunk、64 KiB 总界、背压、阻塞写取消和 abort 时 process termination。
- 本切片完成后运行 locked 平台 tests、strict Clippy、格式与 ADR 索引检查。
- 未启动真实 broker service/HCS MCP guest；Quota/HCS acceptance 按用户要求暂不执行。
