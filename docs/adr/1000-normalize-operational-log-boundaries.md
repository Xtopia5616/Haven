# ADR 1000：统一运维日志与本机路径响应边界

## 状态

已采纳并实施。

## 背景

遵循 ADR 0987、0989 对日志的收敛方向继续复扫时，发现录音/VAD、Agent 执行、ToolRun、MCP/Messaging 和应用启动收尾仍有原始内部错误、模型输入字段、事实谓词或本机清理路径进入日志的边界。部分 Provider 错误虽经 `sanitize_error_text`，仍违反外部错误只记录稳定类别的约定。前端异常路径还把详情与通知正文写入 console 或镜像进 Rust 日志，即使调用方设置 `log: false` 也会重复镜像。LLM 适配器也把 `str.len()` 的 UTF-8 字节数称为字符数，并在 endpoint 日志中保留 URL path。

此外，`open_skills_dir` 成功时把解析后的技能目录绝对路径作为命令响应返回 UI，Tools 页面再将其显示在 toast 中；调用方只需要知道文件管理器已打开。

## 决定

1. Provider/MCP 错误、模型输出解析错误和不可信 payload 错误不写原文或清洗后的正文；改用稳定类别、固定事件描述、字节数或计数。Tauri 外部服务错误经 `log_external_err` 返回清洗后的 UI 文本，同时只记稳定类别；内部 SQLite、IO、队列和 OS 错误只在确有诊断价值时经 `sanitize_error_text` 后写入日志。
2. 日志不写 prompt/context 正文、模型工具参数键、事实谓词、收件目标、动态工具名或本机路径。需要诊断时保留规范实体 ID、阶段、类别、计数和字节长度。
3. Provider/model 身份、受控 MCP profile 名及 endpoint host 可用于区分配置；endpoint URL 的 userinfo、path 和 query 不进入日志。`str.len()`、序列化请求体、响应体及流片段长度统一以 bytes 命名。
4. `open_skills_dir` 的成功响应改为 `void`。页面只显示通用成功提示；实际打开路径仍由后端安全校验并传给文件管理器，不再暴露给 renderer。
5. 前端错误详情与运维日志分离：`reportError` 可保留归一化详情用于 UI；console 与 Rust 文件日志仅记录固定描述和稳定失败类别。`log_frontend_error` 只接受已知类别，不接收错误文案或通知正文；`log: false` 同时抑制前端与后端的重复日志。
6. 现有执行、重试、错误返回、状态事件、通知和确认流程保持不变。

本 ADR 的前端日志边界决定部分取代 ADR 0085 中“镜像相同用户可见错误”的行为；用户可见错误仍走现有 toast 文案路径。

## 备选方案

- 继续将清洗后的 Provider/MCP 错误写入日志：拒绝，因为清洗只能遮盖已知敏感模式，不能保证外部错误正文没有用户内容或内部路径。
- 保留 `open_skills_dir` 的路径响应供 UI 展示：拒绝，因为调用方只需确认打开操作完成，renderer 不需要本机路径。
- 让 logger 接受任意参数并要求调用方自行过滤：拒绝，因为边界无法保证所有调用点都遵守过滤约定；logger 现在只接受固定上下文和摘要字符串。

## 风险

日志对 Provider/MCP 故障的细节较少；命令名、稳定失败类别、阶段和计数仍保留，用户看到的错误详情和执行/恢复行为不变。若后续需要更细的诊断，应增加受控类别或非敏感计数，不能恢复记录外部正文。

## 影响与验证

- 仅改变运维日志字段和 `open_skills_dir` IPC 的成功响应形状；不改变持久化 schema、配置、错误返回或授权决策，不需要数据库/配置重置。
- Rust IPC contract 由生成器更新；UI command wrapper、Tools view 与单元测试改为 `void` 响应。
- 验证通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、Prettier 检查、IPC 生成与合同/事件检查、ADR 索引检查及 `git diff --check`。

## 回滚

可以恢复先前日志字段和 Skills 路径响应，无需数据迁移；回滚会重新允许不受控错误/路径进入普通日志或 renderer。
