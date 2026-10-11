# ADR 0968：由 Session command adapter 校验 transcript 处理结果

## 状态

已接受并实现（2026-10-11）。

## 背景

ADR 0842 将 `process_transcript` 的调用放在 submit coordinator 中；ADR 0948 后又在 coordinator 解析并校验 session/message response identity。submit coordinator 负责请求排队、乐观状态与 adoption 流程，而 Session command adapter 才是 Tauri command 和运行时 response contract 的边界。继续让 coordinator 直接拥有 `invoke` 会把 IPC 细节和工作流状态混在一起。

## 决定

- 将 `process_transcript` 的 `invoke` 与 `ProcessResult` 运行时解析收敛到 `sessionCommands.processTranscript`。该 adapter 调用 `parseProcessResult`，只有校验过的 result 才返回给 submit coordinator。
- submit coordinator 继续拥有提交排队、失败清理和会话 adoption 工作流；不复制 parser 或 command response shape。
- 更新 IPC owner 静态检查，要求 command 在 `sessionCommands.ts` 唯一注册，并确认 adapter 在返回前解析结果。
- 此决定只调整 `process_transcript` owner；`externalRef.ts` 仍按 ADR 0842 直接拥有 `open_external`，录音 controller 仍使用 ADR 0640 规定的窄注入 port。

## 影响

请求与成功响应 wire shape、运行时校验规则和用户可观察行为不变。response identity 在进入 submit state transition 前已验证，错误仍走现有 rejected-submit 清理路径。无需数据迁移。

## 验证

- UI 单测覆盖 Session command adapter 的响应解析。
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`

## 回滚

将 invocation 和已验证的 `ProcessResult` 映射恢复到 submit coordinator，并同步 IPC owner 检查；不得恢复为不校验原始 `invoke` response。
