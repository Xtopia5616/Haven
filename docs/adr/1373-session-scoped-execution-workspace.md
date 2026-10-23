# ADR 1373：将执行工作目录绑定到会话

## 状态

已实施 — 2026-10-23。完成 ADR 1367 / `execution-context-redesign.md` 的第一切片；其余权限模型、资源契约、工具通道和 UI 预设仍是提案。

## 背景

`SecurityConfig.execution_root` 原本同时充当设置页路径、标题区活动目录、Files 相对路径基准、Shell 默认 cwd 和 Agent prompt 信息。切换一个目录会改变进程级活动根，使其它会话的后续执行也受到影响。现有授权 proof 能绑定策略版本，却没有绑定会话的工作目录。

## 决定

- `security.execution_root` 继续由 ConfigService 持有，但其语义收敛为新会话默认工作目录。会话首次加载时将当时的默认值写入 append-only `session_events` 的 `session_context.execution_workspace_bound` 事件；之后目录由会话事件流拥有。没有旧绑定的历史会话在首次访问时捕获当时默认值一次。该配置事件不代表 transcript 时间线已开始；仅有配置事件时，首条 transcript 仍可正常播种。
- 会话目录更新先校验本地路径，并在 Session lifecycle gate 内拒绝正在运行的会话；更新通过后追加事件并绑定 Tools 授权 owner。若追加失败，则恢复原运行时绑定。事件读取按活动 timeline 选最新绑定，并在 rollback/compaction replay 后保留该非 transcript 上下文事件。
- AgentSpawn 子会话和消息分叉会话在 actor 安装前继承父会话的工作目录，并把继承值写入子会话事件流。普通用户新会话仍捕获当前全局默认值。
- Tools 的 AuthorizationEngine 持有进程内会话目录 capability。Files 路径授权、相对路径解析、Shell 默认 cwd/MXC 写根和 Agent prompt runtime context 都按 `session_id` 读取同一绑定。无会话调用仍使用全局默认目录。
- 执行 proof 与确认输入 hash 绑定会话目录 generation。切换目录会使该会话旧 proof 失效，并取消该会话已登记的受管进程；不会改变其它会话或新会话默认值。只修改新会话默认目录不会改变已绑定会话的 proof。ToolRun 捕获启动时的 session proof，若目录在真正执行前改变则旧 proof 失败关闭，不会转向新目录。
- 被删除会话的运行时绑定随删除清理。Settings 中原全局目录选择器现在编辑新会话默认值，标题区选择器编辑当前会话目录。

没有新增数据库表或 schema；复用现有 append-only event 表，因此无需数据重置。既有 `writable_roots`、工具 `allowed_paths`、permission mode、sandbox、network 和 MCP exposure 仍保留原语义，后续切片再决定如何收敛。

## 替代方案

- 继续保留单一全局活动根：无法隔离会话目录，也无法绑定授权 proof。
- 把目录同时写入 Settings 与 session 表：形成双真源，并需要 schema 与恢复/删除逻辑；拒绝。
- 切换目录时让旧 ToolRun 改用新根：可能将已授权操作重定向到未确认的目录；拒绝，旧 proof 失效关闭。

## 验证与限制

新增覆盖包括会话间目录隔离、切换一个会话只撤销该会话 proof、全局默认变更不移动已有会话、AgentSpawn 与 fork 继承、prompt 使用会话目录、目录绑定跨 rollback/compaction 保留、配置事件不阻断首条 transcript 播种、会话绑定恢复及无效/相对路径拒绝。IPC 由 Rust command metadata 生成并检查 TS 合约。

本 ADR 不声称所有会话权限配置已统一，也不改变网络审批、文件资源声明、MCP 调用通道或执行环境模式。Windows 的隔离强度仍由对应 execution backend ADR 定义；本切片只统一工作目录绑定，不把路径绑定误称为 OS 沙箱。

撤回时可回退 Settings/UI 和会话目录命令，并停止读取该事件类型；无需迁移或重置数据库。若保留事件但撤回运行逻辑，它仅是未使用的 domain event，不影响 transcript 或投影恢复。
