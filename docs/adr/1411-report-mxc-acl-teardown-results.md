# ADR 1411：记录 MXC 隔离层与 ACL 清理结果

## 状态

已实施；2026-10-25 workspace 测试、strict Clippy、UI check/1517 项测试/生产构建均通过。Shell 使用 MXC BaseContainer 的方向权限测试通过。AppContainer+DACL 路径的 ACL 前后快照验收仍需在无 BaseContainer 的受支持 Windows 主机上运行。

## 背景

MXC 兼容层可能临时调整授权目录 DACL。Haven 启动时已显示所选隔离 tier、网络能力与 DACL 行为，但没有在进程结束后记录 MXC 清理警告或恢复结果。仅凭启动提示无法区分清理完成、清理时出现警告，或等待/回收失败导致结果未知。

## 决定

- 启动日志与每次终态 Shell 结果分别记录实际 tier、网络 profile、是否要求 DACL 补强。
- 终态等待完成后再次读取 MXC process warnings，并和启动时的 process warnings 对比。MXC 等待正常完成且没有新增警告时记录 `completed_without_reported_warning`；有新增警告时记录 `warning_reported`、警告数及清洗后的摘要；未使用 DACL 补强时记录 `not_applicable`。
- MXC wait 出错时写入错误日志，明确记录恢复状态为 `unknown`。用户可见结果同时展示最终状态；取消或策略撤权会先终止并等待 MXC 清理，再返回取消结果。
- `completed_without_reported_warning` 表示 MXC wait 已完成且未报告新增清理警告；它不声称 Haven 独立逐 ACE 验证了 DACL。真机验收另需比较执行前后的授权目录 DACL。
- 自动化 Windows 用例在兼容层 DACL 主机上验证越界写仍被拒绝、DACL SDDL 前后相同，并验证结构化结果与用户提示。

## 替代方案

- 只在启动时说“退出后恢复”：拒绝，缺少进程结束后的清理证据。
- 把任意 MXC warning 直接当作 DACL 恢复失败：拒绝，warning 文本没有结构化的 ACL 专属错误码；应如实报告 `warning_reported` 并保留清洗后的摘要。
- 只靠日志、不向 Shell 结果回传终态状态：拒绝，用户无法从本次执行结果了解恢复状态。

## 影响与回滚

Shell 工具结果增加可选 `sandbox_isolation` 诊断对象，不改变授权或进程策略，不涉及持久化数据或配置重置。回滚时若移除诊断字段和 UI 提示，必须保留 MXC wait 后的 warning 检查以及 unknown 状态日志，避免再次静默丢失 ACL 清理异常。

## 验证

- 平台单元验证 tier/profile/ACL 状态分字段输出，启动 warning 不误计为 teardown warning，wait 后新增 warning 会标为 `warning_reported`。
- Shell 端验证取消路径会 wait 收尾并把终态诊断附加到工具结果。
- `cargo test --workspace --locked`（Tools：984 passed、15 ignored）、平台恢复状态测试、Agent 范围提示测试、`cargo clippy --workspace --locked -- -D warnings`、Svelte check/UI 测试/生产构建与 ADR 索引检查通过。
- AppContainer+DACL 真机用例会比较 Shell 授权目录执行前后的 SDDL。当前 Windows build 29683 主机使用 BaseContainer，不满足无 BaseContainer 的运行条件；该用例因此保持 ignored，ACL 前后快照验收仍开放。
