# ADR 0869：按 fallback 语义命名 Session 标题读取

## 状态

Accepted — 2026-10-10

## 背景

`SessionStore::session_title` 只读取显式持久标题；`SessionStore::session_display_title` 则读取显式标题或原始 `input_text`，供 `end_session` 在 executor 会话缺失时生成完成通知标题。另一个 App 层的 `DesktopNotifications::session_display_title` 负责通知显示，包含缓存、非空字段与 session ID fallback。相同的 `session_display_title` 名称因此分别表示持久字段读取和带缓存的 App 展示解析，容易让调用方误判字段策略及 owner。

## 决定

- 将 `SessionStore::session_title` 改为 `get_session_title`，明确这是按 session ID 读取一个持久字段投影；只返回显式标题，不自行回退到输入。
- 将 `SessionStore::session_display_title` 改为 `get_session_title_or_input`，明确该查询使用 `title.unwrap_or(input_text)`；session 不存在返回 `None`，查询错误保持原样传播。
- App 的 `DesktopNotifications::session_display_title` 与 `end_session_display_title` 保留在 App 层，各自管理通知缓存、非空展示 fallback、executor 优先级及错误降级，不把不同显示策略合并到 Memory。
- 删除旧 SessionStore 方法名，不提供兼容 alias；同步更新所有生产调用与测试名称。

## 替代方案

- 把两种 SessionStore 读取继续统称为 `session_display_title`：拒绝，名称没有表达 `input_text` fallback，且与 App 通知 resolver 撞名。
- 合并 SessionStore 与 App 的标题 resolver：拒绝。Memory 只负责持久字段查询；通知 resolver 还依赖进程缓存和非空 fallback，`end_session` 则优先使用 executor 数据并保留其空标题及查询失败契约。
- 让所有显示场景共用非空 title/input/session ID 策略：拒绝，这会改变 `end_session` 既有的 empty-title、缺失记录和查询错误行为。

## 影响与验证

- 仅重命名 Memory、Agent、App 内部 Rust API，并更新命名规范与重构路线图；无 IPC、数据库 schema、配置、持久数据或用户可见行为变化，不需要数据重置。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked --tests` 与 `cargo clippy --workspace --locked -- -D warnings` 通过；按项目指示仅编译测试目标，没有运行测试套件。

## 回滚

若该字段投影被删除，可同时删除两个 SessionStore 读取端口及其调用方；不要恢复模糊的 `session_display_title` 别名。无持久数据需要重置。
