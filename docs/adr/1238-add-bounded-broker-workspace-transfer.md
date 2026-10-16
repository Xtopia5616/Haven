# ADR 1238：为 broker 实现有界 workspace 文件传输

## 状态

平台侧的 workspace snapshot sender/receiver 和 capability-relative directory listing 已实施并通过单元测试；尚未连接 named-pipe client/service、私有 staging owner 或 HCS provider。HCS quota runtime gate 仍由 ADR 1231 阻断生产执行。

## 背景

ADR 1231–1232 定义了版本化、有界的 broker 控制帧与认证 named pipe；协议已保留 workspace entry、workspace chunk 和 snapshot completion 消息，但尚无实际文件树编码/接收实现。HCS 只能接收 broker 自己持有的私有输入 staging，IPC 不得携带 workspace 根路径，也不能把单个大文件整体缓冲在内存中。

## 决定

- sender 只接收 capability-pinned 的 snapshot directory，通过相对路径遍历文件树；目录枚举拒绝重解析路径，条目按路径顺序输出。reparse point、非普通文件、无效 Unicode/协议路径、超出 caller-provided entry/depth/file/total-byte 限额和取消均 fail closed。
- 每个条目先发送经过协议校验的相对路径、类型、长度与 SHA-256。文件数据以最多 32 KiB 的 `WorkspaceChunk` binary frames 流式传输；sender 使用同一打开的文件句柄先计算摘要、再回到起点发送，并复核实际发送长度和摘要，检测并发源文件变化。
- receiver 要求 destination 是空的 pinned capability，只接受目录/文件 entry 与其有序文件块；目录必须先于子项。receiver 为每个文件流式写入新文件，不缓存整个内容，按长度、SHA-256、entry/depth/file/total-byte 限额和最终 snapshot summary 校验。
- 不完整文件在 receiver 丢弃时删除；发生传输错误后，调用方必须删除整个本次 staging attempt。receiver 不持有 scratch owner，也不擅自清理调用方目录中此前已接收的条目。
- 此切片不创建 staging 根、不连接 named pipe、不启动 HCS、不生成输出 review，也不改变服务当前 `quota_unattested` 响应。后续 provider 必须将该 transfer engine 接到 broker-owned attempt lease，并确保 HCS 使用的映射 capability 与 staging owner 同生命周期。

## 替代方案

- 将整个 workspace 文件树序列化进控制 JSON：拒绝，控制消息有独立小上限，且整树缓冲会形成不受控内存峰值。
- 通过 IPC 发送 host root path 让 broker 自行读取：拒绝，会把桌面进程文件权限扩展为 broker 的 ambient authority。
- 直接将 canonical workspace 映射给 HCS：拒绝，会跳过 attempt snapshot 和路径/文件类型限制。
- 只依赖 entry 的声明长度而不核验传输内容：拒绝，会让损坏、错序或中途变化的数据进入 guest 输入树。

## 影响与迁移

该切片增加平台内部的 capability-relative 目录枚举，以及 sender/receiver 实现与测试。不改变 named-pipe 线上字段、协议版本、数据库、用户配置或工具权限，不需要数据/配置重置。当前 broker 行为不变，因为 server 仍在传输开始前 fail closed。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 单元测试覆盖嵌套文件树 round trip、大文件分块、空文件、流式摘要/长度、摘要错误、未完成 snapshot、深度限制和传输前取消。reparse point 负向用例会在创建 symlink 需要但当前进程缺少的 Windows privilege 时提前返回；本机报告 `ERROR_PRIVILEGE_NOT_HELD`，因此该分支未在本轮实际执行。
- 未运行 HCS runtime 或 guest 超额写入验收；本切片也未验证 named-pipe/service 集成。

## 回滚

删除 `workspace_transfer` 模块与 `DirectoryCapability::entries_at` 即可回滚；该模块尚无生产调用方，回滚不会改变 broker service 或工具行为。
