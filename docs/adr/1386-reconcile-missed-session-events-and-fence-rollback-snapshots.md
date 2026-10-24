# ADR 1386：自动补齐漏收的会话事件并隔离 rollback 快照

## 状态

实现及自动化门禁已通过；持续聚焦丢事件的隔离 Windows 验收已通过（2026-10-25），恢复与 rollback 并发交互仍 Open。对应架构审查 R05/R12。

## 背景

Agent durable UI publisher 与前端 reducer 已按事件 sequence 去重，但 Tauri 通知是实时通道：窗口一直可见时若一个提交事件静默漏收，后续没有可触发恢复的 focus transition。另一个竞态是恢复请求先于 rollback 读取投影、却在 rollback 后返回，可能把旧 timeline 覆盖到新分支。

## 决定

1. App 增加只读 `get_session_event_sequence_watermark(session_id)`，复用 Memory `SessionStore` 的 append-only sequence 查询。该命令只返回一个无符号水位，不读取 transcript、usage 或交互投影。
2. 聊天页可见期间每 5 秒检查当前会话水位；遇到后端水位前进或本地观察到 sequence 缺口时，才读取恢复投影。恢复前后水位必须相同，且投影内水位必须与之一致；并发提交期间丢弃本次结果，后续 tick 重试。单个请求在途时不重叠发起。
3. Reducer 记录已观察/快照水位和 sequence 缺口。新快照不得覆盖更高的 live 水位；接受快照后，快照水位内迟到的 durable UI 通知会被忽略，防止重建后又插回已覆盖内容。
4. 恢复请求携带进程内 session snapshot version。rollback、continue、replay reset 和 transcript 清理会使旧请求失效；rollback 成功后只接受新版本请求取得的投影。版本只在存在在途快照时保留。
5. 正常 Tauri 实时事件路径、durable event 格式和数据库 schema 不变。sequence 跳跃可能来自没有 UI 卡片的 durable event，最多额外触发一次恢复读取，不改变数据语义。

## 替代方案与边界

- 只在 `window.focus` 时恢复：不能覆盖窗口持续聚焦期间的静默丢失。
- 每 5 秒读取完整 transcript：没有丢失事件也持续重建投影，读取开销高于先查轻量水位的方案。
- 仅按 snapshot event watermark 合并：不能区分 rollback 前已经启动、rollback 后才返回的请求；因此另以进程内版本隔离在途结果。

## 验证

- UI reducer 回归覆盖漏序检测、快照补齐、旧快照拒绝及快照水位内迟到事件过滤。
- Chat controller 回归覆盖漏收事件补齐、cursor 未变化时跳过完整恢复请求，以及 pre-rollback 请求不能覆盖 rollback 后投影。
- Reconciler 回归覆盖页面可见性、当前会话选择、轮询不重叠与 dispose 清理。
- `scripts/check-ipc-contracts.ps1`、`scripts/check-adr-index.ps1`、`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run` 与 `corepack pnpm run build` 均通过。
- `cargo tauri dev` 在隔离的 `HAVEN_DATA_DIR` 下启动成功，Windows WebView 首屏可用。2026-10-25 隔离验收中，Haven 主窗口保持前台可见时由独立测试进程直接追加未广播的 durable event；UI 从 cursor 12 检出 seq 13 并应用稳定恢复快照，验证持续聚焦丢事件后的补齐。
- 恢复请求与 rollback 并发的真实 UI 操作仍未执行；旧快照 fence 有自动化覆盖，§5.1 的 Windows 并发交互项继续 Open。

## 数据、回滚与风险

不新增或修改持久数据、配置、数据库 schema 或既有事件 payload；无需数据重置。撤回时删除水位命令和 renderer reconciler，并恢复原快照 merge 路径。自动化测试不替代 §5.1 规定的真实 Windows 连续聚焦事件丢失、rollback 与恢复请求并发、真实 WebView/SQLite 和发布生命周期验收。
