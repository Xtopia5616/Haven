# ADR 1385：模型与媒体配置使用共享代次原子发布

## 状态

已实施 — 2026-10-24。对应架构审查 R03。

## 背景

R02 的 Router 应用先更新 Agent，再更新 Tools 的 `PlatformRuntime`。两次更新之间，新 Agent 请求可能使用新 router，而 media、voice ingress、文件摘要或独立搜索仍持有旧 router/专用媒体客户端。Tools 自己的 `PlatformRuntime` 是原子快照，但 App、Agent 与 Tools 之间没有共同的发布点。

## 决定

1. `haven_llm::ModelMediaSnapshot` 将 `config_version`、process-local `generation`、router、STT/OCR/image-generation/TTS clients 与 `MediaConfig` 收为一个不可变组合。`ModelMediaRuntime` 只提供快照读取；创建时返回的 `ModelMediaPublisher` 是唯一写能力，保存在 App 的 `ApplicationRuntime`。Agent、Memory 与 Tools 只收到 read handle。
2. App 从同一个 `ConfigSnapshot` 准备完整组合，成功后由 `RuntimeConfigCoordinator` 通过一个 publisher 操作切换共享 `Arc`。一次 Agent/Memory LLM 请求、一次媒体操作、一次文件摘要、一次独立搜索及一次应用转写在第一次 await 前捕获快照；在途操作继续持有旧快照，新操作读取新快照。
3. Memory 的 router consumers 改为读取共享 handle。Tools 的 `PlatformRuntime` 持有同一只读 handle；媒体工具在每次执行时重新捕获当前代次，并从该快照构建 transcriber、OCR、image-generation、TTS 和媒体能力。文件摘要、独立搜索、管理诊断与 app voice transcription 也从一次捕获读取 router 与相关媒体配置，避免旧 catalog 中的 provider 句柄在配置切换后继续生效。
4. 内部工具目录是由当前模型/媒体能力派生的投影，仍在原子代次发布后重建。若目录重建失败，ToolRegistry 保留原目录快照；错误及 Settings retry reason 明确给出已原子发布的 `config_version` 和派生目录失败。旧目录中的媒体操作执行时仍捕获共享 handle 的当前代次；当前不可用的操作返回不可用结果，尚未重建的新能力在重试目录前不可调用。
5. 最小原子边界是单次 LLM 请求或单次媒体/provider 操作，不扩展为整个 ReAct turn。一个 turn 可在两次模型请求或模型请求与后续工具执行之间跨越配置发布；在途调用不取消。后台任务与 session 执行上下文按代次绑定仍是后续工作。MCP 重连、热键和其他系统副作用也不属于这一代次事务。

## 替代方案与边界

- 交换 App 中 Agent/Tools 的发布次序：拒绝；任一顺序都在两次独立发布之间留下相反的混合窗口。
- 逐个调用 Agent、Memory 与 Tools setter：拒绝；它会保留多个配置真源和手动同步责任。
- 把 publisher 一并交给 Agent/Tools：拒绝；消费者只能读共享快照，App composition owner 单独持有发布能力。
- 将整个 ReAct turn 绑定到代次：当前不采用；会改变工具重试、补充输入和后台任务的生命周期，需要独立定义 turn/ToolRun 语义。

## 验证

- `haven-llm` 测试交替发布 A/B，并在并发读期间断言 router 与媒体配置始终配对；已捕获的旧快照在发布后保持不变。
- `haven-tools` 测试让 A 代次的专用 STT 在途阻塞，发布 B 后确认 A 调用仍以 A 完成、下一次操作改用 B。
- Tools catalog failure injection 验证共享 generation 已完整发布时，旧 catalog 保持原子快照且 read handle 可见新版本。
- Agent router reload 回归覆盖标题、记忆推断、prompt recall、vector recall 与 embedding index 的共享发布点。
- App prepare 测试确认准备出的快照保留所输入配置版本的 media config 与 router。
- `cargo fmt --all -- --check`、`cargo check --locked -p haven-tools -p haven-app-binary`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 均通过；`scripts/check-adr-index.ps1` 确认 1283 条 ADR 唯一编号记录及本地链接有效。
- 没有执行 Windows 安装包/真实环境验收；发布验收仍按路线图 §5.1 开放。无 UI、Tauri DTO、数据库或持久配置格式变化，因此本切片不要求 UI/IPC 生成门禁或数据重置。

## 数据、回滚与风险

代次只存在于进程内，无 schema、配置文件、session event、IPC 或密钥变化。回滚只需回滚代码并重启；启动从当前保存配置构造一个新共享代次。目录重建失败时用户可按 R02 的显式 reapply 重试，不需要改配置值。旧代次会被正在运行的操作以 `Arc` 持有直到该操作结束；不会通过发布动作撤销已发送的请求。
