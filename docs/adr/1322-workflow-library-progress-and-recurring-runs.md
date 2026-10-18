# ADR 1322：Workflow 定义库、运行进度与周期调度

## 状态

已采纳；扩展 ADR 1321 的运行实例能力，并仅替代其中“不建立 Workflow 定义库”的范围决定。

## 背景

ADR 1321 已让 Workflow 在单次 ToolRun 中持久化、恢复并等待嵌套确认，但与传统 ToolRun 体验相比仍有四项缺口：用户不能保存和复用命名定义；后台运行缺少逐节点进度；失败运行没有检查与安全续跑入口；调度只支持立即或单次时间触发。

## 决定

1. **版本化定义库：**新增用户级 Workflow 定义库，支持命名、查看、编辑、复制、删除和从 UI 启动。当前版本以 JSON 编辑器保存已有的静态 DAG schema；更新必须携带当前 revision，过期修改会冲突失败。启动和创建周期计划时复制选定 revision 的定义快照；之后编辑或删除定义不改变已创建运行和计划。数据库 owner 保存定义，不保存授权 receipt；每次执行仍按 live catalog 与实时策略重新授权。当前没有独立的参数表单，动态值仍需通过 DAG 中的 literal/result 映射表达。
2. **步骤进度：**ToolRun 持久行增加有界的当前节点与 completed/total 计数，并发布 typed progress event。事件不携带参数、原始结果或 observation。运行恢复时，ToolRun 行中的进度只作 UI 投影；子调用 journal 仍是确认续接和崩溃恢复的唯一权威。
3. **检查与续跑：**ToolRun 详情展示失败原因和安全重试资格。只有 Workflow handler 明确报告失败发生在 child handler 启动前、且没有任何已完成 child call 时，才允许创建新的 ToolRun 并从定义起点重跑。已完成 child call、已启动但没有完成 checkpoint 的未知操作，以及不含可信安全标记的失败均不提供重试入口；用户可先检查失败详情，再决定是否从定义库启动新的完整运行。此切片不会把部分完成的结果注入新运行，也不会自动重放副作用。
4. **周期调度：**新增用户可见的 Workflow schedule owner，按 UTC 固定间隔运行；计划的定义 revision、session、间隔锚点、下次触发时间和当前 occurrence 均持久化。每个 occurrence 是独立 ToolRun，继续复用确认、取消和 completion outbox。任一时刻一个 schedule 最多运行一个 occurrence；若执行跨过多个周期，跳过过期 tick 并推进到下一个未来锚点，不追赶、不重叠。计划可暂停、恢复或删除；删除会让当前 occurrence 独立完成但不再触发后续。删除 session 或到期时发现关联 session 已结束会停止后续 occurrence。当前可新建计划替代旧计划，不提供原计划的间隔编辑。此切片不支持 Cron、本地墙上时间/DST、依赖事件触发或强制终止正在运行的 handler。
5. **保留有界 DAG：**定义库不改变 Workflow 无分支、无循环、无并行、无动态工具名、无隐式重试的静态执行模型。代码定义、步骤数、参数与运行结果仍受进程预算约束。定义库不能扩大当前 session 的 MCP Workflow allowlist。
6. **持久契约：**新增 Workflow definition/schedule 表与 ToolRun 关联字段，统一由 `haven_memory::schema::init_schema` 创建；schema 版本递增，旧版本仍按项目政策拒绝打开并要求按 release-reset 指引备份后重置。定义 ID 与 schedule ID 使用新的 canonical ID 前缀；运行 occurrence 继续使用 `toolrun-*`。

## 替代方案

- 将定义放入 `config.toml`：拒绝；定义拥有独立编辑/版本/删除生命周期，不属于运行配置。
- 定时重复复用同一个 ToolRun ID：拒绝；每次 occurrence 都是单独的执行与终态，不能把 terminal 状态回退为 waiting。
- 进度事件发送工具参数或 observation：拒绝；这会将敏感调用数据复制到 UI 事件流。
- 允许用户忽略 unknown outcome 后从该节点重跑：拒绝；外部副作用可能已经发生，当前 capability 没有通用幂等证明。
- 赶上所有错过的周期或允许 occurrence 并发：拒绝；会在应用长时间关闭后产生无界积压或重复并发副作用。

## 影响与重置

Workflow definitions、参数与 schedules 新增 durable 表；ToolRun schema 为进度/关联与人工续跑 provenance 增加字段。按当前版本不提供运行时数据库迁移；升级前须备份并删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`，将清除会话、记忆、授权、Workflow 定义、周期计划、ToolRun 历史与用量。config.toml 无需重置。

## 验收

- 定义 CRUD、revision 冲突、参数 schema/默认值/必填边界、删除与已启动快照隔离。
- ToolRun 节点进度的创建、更新、恢复投影、事件顺序、取消、敏感字段排除与 UI 展示。
- 安全重试只接受失败发生在 child handler 前且没有已完成 child call 的 Workflow；partial/unknown outcome 和已完成步骤均拒绝重试。
- 周期计划跨重启重建、暂停/恢复/删除、occurrence 去重、活动重叠、错过多个 tick、session 删除或结束、确认等待、取消与终态更新。
- Windows 实际验收定义保存、定时触发、运行中 UI 进度、失败检查与人工续跑。
