# ADR 0091：内置工具面精简与本机操作补齐

- 状态：Accepted
- 日期：2026-09-06

## 背景

内置工具的稳定顶层边界已经由 `files`、`system`、`window`、`actions` 等聚合工具承担，
但仍有几处实现层暴露和契约漂移：`env`、`power`、`registry` 只是 `system` 的内部 scope，
过期的 `messaging`/`haven` 路由名仍留在权限参数表中；`actions` 无法取消后台任务，
`files` 无法创建缺失的目录，窗口操作也不能按已经返回的 PID 聚焦或关闭。
此外，环境变量列表和写入结果会把值直接返回给模型，存在泄漏凭据的风险。

## 决定

1. 保持稳定的顶层工具名，不拆分或重命名 `agent`、`system`、`files`、`audio`；把
   `env`、`power`、`registry` 收紧为 `system` 的私有实现模块。
2. 权限路由只保留实际模型入口和 operation-aware capability：删除过期的 `messaging`、
   `haven` 路由，补齐 `actions`、`agent` 和六个 `haven_*` capability 的 operation key。
3. `actions(operation=cancel, action_id=...)` 只允许取消当前 session 所拥有的任务，
   风险为 medium，并复用 `BackgroundActions::cancel_for_session` 的幂等语义。
4. `files(operation=create_dir, path=...)` 使用现有路径清理和安全网关，创建目录树，风险为
   medium；`window` 的 `focus`/`close` 接受 title 或 PID，二者同时提供时先按 PID 过滤再按标题匹配。
5. 环境变量 `list` 只返回名称；`get` 对 credential-like 名称返回 `[masked]` 并标注
   `masked=true`；`set` 成功结果不回显 value。未命中的 `get` 保留 `value=null`。

## 替代方案

- 继续公开 `env`、`power`、`registry`：拒绝，这些不是模型能力边界，只会扩大工具目录和权限维护面。
- 用新的取消 dispatcher：拒绝，已有按 session 归属校验的后台任务取消实现。
- 允许环境变量工具原样返回值：拒绝，模型输出、日志或重试链路可能传播凭据。
- 立即拆分 `system` 或重命名 `agent`：暂缓，等调用数据和权限迁移收益足够明确后单独做契约变更。

## 影响与验证

模型工具目录增加两个明确的本机操作（后台任务取消、创建目录），窗口操作获得 PID 目标能力；
环境变量读取结果变得更少但更安全。旧的过期权限键不再产生 operation 级授权；稳定工具根键和
已有配置迁移保持不变。

验证包括 `haven-common` 权限 key 测试、`haven-tools` builtin/schema/security 测试、严格
Clippy、workspace 测试、UI check/test，以及格式化检查。

## 回滚

回退本 ADR 对应提交即可恢复实现。由于新 operation 没有持久化数据迁移，回滚不需要数据库重置；
环境变量脱敏行为回滚前应确认没有把模型上下文当作秘密存储。
