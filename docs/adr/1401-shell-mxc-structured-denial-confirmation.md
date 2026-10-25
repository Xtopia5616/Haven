# ADR 1401：Shell 使用 MXC 结构化拒绝记录发起一次性资源确认

## 状态

已采纳并实施；Windows MXC BaseContainer 真机验收通过。AppContainer+DACL ACL 恢复仍需在不支持 BaseContainer 的 MXC 主机上单独验收。

## 背景

WorkspaceWrite 下的 Shell 由 MXC ProcessContainer 隔离。范围外访问由操作系统立即拒绝；Haven 无法在 Shell 的每次文件系统调用处暂停进程并弹出实时授权框。MXC `captureDenials` 提供了结构化的 deny-and-record 能力：访问继续被阻止，进程结束后返回去重后的拒绝资源、资源类型、访问类型、进程号、时间戳、计数与截断状态。

此前 Shell 只能给出命令退出结果，模型和用户无法据此判断被拒绝的具体资源，也无法直接从失败结果继续完成原操作。

## 决定

1. MXC probe 确认当前隔离层支持 `captureDenials` 后，Shell 使用 `Block` 模式采集拒绝记录，不保留 ETL。若 probe、启动或能力确认失败，仍以原隔离策略启动，并在结果中说明拒绝记录不可用；不得因此放宽隔离。
2. 进程结束后读取 MXC 结构化 JSON；最多读取 4 MiB、向调用者最多返回 64 项。结果包含捕获状态、去重后的资源/访问类型对总数、截断和完整性标记、资源、资源类型、访问类型、PID、十进制字符串 FILETIME 和捕获错误。校验文件摘要与进程元数据；不完整、超限或不一致时，不据此发起授权重试。
3. 仅当记录完整、未截断，且文件系统拒绝能映射到至多 64 个可用的本地目录范围时，Haven 才创建一次性 `ResourceBoundaryExtension` 确认，与最多返回的 64 条 MXC 记录一致。文件拒绝映射到其父目录；读取/执行类使用只读范围，写入类使用读写范围。MXC ProcessContainer 使用目录根实施 ACL，因此弹窗明确列出实际目录和访问类型。
4. 确认仅绑定当前 Shell 调用的结构化资源，不修改保存的策略。用户批准后重跑完整原命令，并将首轮 `sandbox_access` 记录附在最终工具结果的 `previous_sandbox_access` 中。其它网络、UI、未知或其他资源类型继续按既有策略阻止。弹窗说明首轮命令可能已产生部分副作用，重跑可能重复这些副作用。
5. 保持全局文件路径根、工具级路径/写入范围、沙箱模式和执行授权证明的现有约束；普通已确认的工作目录仍可按原路径授权。Shell 重试仅将显式 `_resource_extensions` 作为新增范围，并只在重试收据仍包含工作目录读资源时恢复该工作目录；不从未绑定资源推导进程根。
6. 不增加持久配置、数据库字段、Tauri 命令或数据迁移。嵌套确认摘要是可选字段，缺失时按旧行为处理。

## 替代方案

- 每次拒绝立即弹窗：MXC 的 `captureDenials` 只在进程运行时记录并在结束后汇总，不能暂停每一次文件访问供 Haven 作出实时决定。
- 首轮访问失败后直接放宽范围或自动重试：拒绝，会绕过用户确认，并可能重复首轮已完成的写入。
- 将 MXC 未知/网络/UI 拒绝映射为文件权限：拒绝，不同类别由独立安全边界控制，不能把它们混为目录授权。
- 只显示 Shell 退出码：拒绝，会丢失 MXC 已提供的结构化诊断与受控续行能力。

## 影响与回滚

支持 captureDenials 的 Windows 主机可在首轮阻止访问后收到带具体目录的单次确认；其它主机保留原隔离和命令结果，只会报告诊断能力不可用。Shell 命令的第一次运行仍会被 MXC 阻止越界访问，且可能已经产生其他副作用；授权后按用户确认的目录访问类型重跑。

回滚时移除 Shell 的嵌套资源确认和重试注入，但可以保留结构化拒绝结果作为诊断。没有数据库或配置需要重置。

## 验证

- Windows build 29683 的 MXC BaseContainer 主机运行 `workspace_write_shell_captures_denials_before_any_retry` 与 `workspace_write_shell_resource_extension_requires_confirmation_and_retries_once`：首轮外部写入被阻止且捕获完整，首轮副作用只发生一次；资源扩展必须取得 `ResourceBoundaryExtension` 确认，批准后完整命令只重跑一次，副作用计数为两次。
- 同一主机运行 `workspace_write_shell_is_confined_to_tool_write_roots_by_mxc`、`workspace_write_shell_applies_tool_read_only_policy_by_mxc`、`workspace_write_shell_enforces_readonly_subpaths_and_denied_paths`、`workspace_write_shell_blocks_junction_aliases`、`workspace_write_shell_refuses_preexisting_hardlink_aliases`、`workspace_write_shell_cannot_create_an_outside_hardlink_alias` 与 `workspace_write_shell_cannot_alias_a_readonly_subpath_into_a_writable_path`：全部通过。预置 hardlink 曾在 MXC 下成功修改根外文件；Shell 现对有效写根执行有界 hardlink 检查并在 spawn 前拒绝该目录，详见 [ADR 1409](1409-refuse-mxc-shell-hardlink-escapes.md)。
- `shell_with_domain_restrictions_is_blocked_before_network_confirmation` 通过：全局或 Shell 工具级域名限制下，opaque Shell 联网在授权阶段阻止，MXC 不会启动。
- 当前主机使用 MXC BaseContainer；每次真实运行均报告“MXC 不会修改授权目录的 DACL”。因此本轮没有改变 ACL，也未声称 AppContainer+DACL ACL 恢复已验收。需在 BaseContainer 不可用、兼容隔离实际选择 AppContainer+DACL 的 Windows 主机上运行 `workspace_write_shell_is_confined_by_appcontainer_dacl_compatibility` 并检查 ACL 恢复。
- 完整与截断捕获、捕获不可用降级和 MXC 非文件拒绝保持阻止仍需保留其既有 mock/负向测试；上述 BaseContainer 真机结果不替代 AppContainer+DACL 验收。
