# ADR 1371：扩展生命周期 Hooks 的管理与事件覆盖

## 状态

已接受并实施 — 2026-10-23。

## 背景

ADR 1356 建立了外部命令 Hook、项目目录信任和安全网关，但用户需要手工修改配置并重启；事件覆盖仅到工具执行、会话启动和压缩前；项目目录信任没有设置入口，运行结果也缺少用户可见诊断。

## 决定

在 Settings 中增加独立的 Lifecycle Hooks 分类。用户级 Hook 使用专用读写命令和 ConfigService typed patch，不加入普通 SettingsSnapshot/SettingsPatch。每次事件都读取权威配置快照，因此保存后立即生效。项目根通过原生目录选择器规范化并信任/撤销；项目 Hook 文件仍只对完全相等的活动工作区根生效。

增加 `run_start`、`run_end` 和 `post_compact` 事件。`session_start` 在一个 SessionActor 首次接受 Agent run 时触发；Haven 重启并重建 actor 后可再次触发。`post_compact` 只在 CompactSummary transcript 写入成功后触发。`run_end` 返回静态、无内容的 outcome。Hook 顺序固定为用户级在前、项目级在后；列表保持配置顺序。PreToolUse 是唯一可拒绝事件且失败关闭，其它事件为失败继续的通知。

工具 matcher 支持有长度上限的 `*` 字符通配，并按大小写敏感比较；不引入正则引擎。设置页同时显示当前沙箱/网络策略和进程内事件诊断。诊断仅包含计数、结果枚举和静态错误码，不暴露命令、参数、会话数据或进程输出。

外部命令继续沿用 Haven 当前 FullAccess + NetworkPolicy::Open 的安全约束、最小环境、进程树清理、事件/输出/输入上限与执行时策略复验。信任项目目录不授予命令绕过安全网关的权限。

## 替代方案与边界

将命令配置并入普通 SettingsPatch 会暴露可执行命令与 Hook 信任根给通用设置保存流程，故采用专用 typed command。自动从项目仓库发现并信任 Hook 会让仓库内容获得本机进程执行入口，故仍要求显式目录信任。`SessionEnd` 和模型通知类事件暂不纳入：当前会话 actor 的销毁不等于会话完成，错误映射会让 Hook 生命周期产生歧义；应在拥有明确 session terminal transition 的边界单独设计。

配置是用户本地 TOML，新增字段使用默认值，不改数据库、事件存储或 ID 契约，无需重置持久数据。已有 Hooks 的安全模式、命令传参和项目信任规则保持不变。

## 验证

更新 IPC 生成契约、Rust 正/负测试、设置页类型检查与 UI 测试、Rust workspace 测试和严格 Clippy；Windows Hook 进程边界使用现有 Windows 专项回归。

## 回滚

回滚代码即可。用户可移除 `config.toml` 中的 `hooks.run_start`、`hooks.run_end`、`hooks.post_compact` 和 `trusted_hook_workspaces` 配置；它们不会被其他持久数据引用。
