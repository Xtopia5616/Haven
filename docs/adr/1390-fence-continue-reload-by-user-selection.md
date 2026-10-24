# ADR 1390：隔离继续生成期间过期的会话选择

## 状态

已实施 — 2026-10-24。

## 背景与证据

分领域审查 R12 要求覆盖快速切会话后旧请求返回。`ChatSessionController.handleContinue` 在 `continueSession` 返回后校验选择版本，但随后等待 `reloadSessionMessages`，期间用户仍可切换会话。原实现既未向重载传入选择版本，也未在重载结束后再次校验。`submitTranscript` 在入队时捕获当前会话，因此旧操作可能将“继续”或原始输入提交到新选中的会话，或新建意图下的新会话。

新增 12 个回归场景在修复前全部失败：两种续写策略 × 重载成功/失败 × 切换其他会话/新建/切走再切回。失败观察包括过期操作仍提交、应用旧快照和修改自动跟随状态。

## 决定与不变量

1. 选择版本继续由 `SessionReducer` 独占管理；不新增状态 owner、队列或 controller。
2. 将发起时的选择版本传给现有恢复读取入口；过期快照不得应用，即使用户已切回同一会话。
3. 重载成功或失败后，都重新核对选择版本和活动会话，才允许 replay reset、自动跟随与后续提交。新选择发生后，旧操作正常结束并释放 pending 标记。
4. 已发送的后端 `continueSession` 不补偿或重放；普通消息的 submission token、按会话串行和独立重复输入语义保持原契约。

## 替代方案与停止条件

- 仅比较当前 session ID：无法覆盖切走再切回，也无法区分同会话的新选择意图。
- 重载期间禁止切换：引入多余交互限制，现有选择版本足以隔离结果。
- 扩大为通用动作状态机：本缺陷不需要新 owner；只修复一条完整调用链，不按 controller 体量拆分。

## 影响、风险与回滚

仅影响 UI 继续生成编排；无 Rust、IPC、数据库、配置或持久事件契约变化，无需重置。用户改变选择后，已经完成的后端继续命令可能仍改变原会话状态；本切片保证旧 UI 操作不会再向新选择提交。真实 WebView 并发交互仍由路线图 §5.1 验收。

回滚时还原 controller 的选择校验、对应测试与 UI 契约；会重新开放上述竞态。R12 其余动作、调度容量及会话配置代次不在本切片内。

## 验证

- 修复前：定向测试 12 failed、20 passed；修复后 32 passed。
- 最终门禁使用 Node.js 24.21.0、pnpm 12.12.1：`corepack pnpm --dir ui run check` 为 0 errors、0 warnings；`corepack pnpm --dir ui run test:run` 为 169 files、1492 tests 全部通过。
- 全量测试输出了 `TimeoutNaNWarning`，无失败；未将该警告归因于本切片，也未扩大到无关 timer 修改。
- `scripts/check-adr-index.ps1`、`git diff --check` 通过；复核了 camelCase 命名、选择版本 owner 和显式 submission token 保留语义。
- 无 Rust、IPC、路由装配或构建配置变化，未运行 Rust 门禁与生产构建；未执行真实 Windows UI 并发验收，不关闭发布 Gate。
