# ADR 0356：架构路线图状态核对与文档校准

- 状态：已采纳（2026-09-26）
- 范围：仅文档；`AGENTS.md`、`docs/development-standards.md`、`docs/architecture-refactor-roadmap.md`、`docs/architecture.md` 与 ADR 索引
- 审查基准：执行核对时的 HEAD `0231a1d` 及其可达提交历史

## 背景

路线图与架构文档包含早期切片的“仍待/未完成”快照。后续 ADR 已完成若干相关域，导致 Phase 3、Phase 6、Phase 7 和 Phase 8 当前状态、验收描述与实现不再完全一致。Phase 3 的旧切片还把 ReActEngine 与 MemoryWorker 的 raw Database ownership 写成未完成；ADR 0299 与 0312 后已分别移除这两条生产路径，MemoryService 仍私有保留 backing handle。该核对只整理已有提交与实现证据，不新增生产行为或架构决定。

审查基准提交 `0231a1d` 的 ADR 索引最高项为 0354。该提交的 git tree、可达提交历史和文档中均没有 ADR 0355；其 app event payload runtime validation 已作为 ADR 0346 的后续变化记录。故本 ADR 不推造 0355 文件或结论。

## 决定

1. **Phase 6**：ADR 0354 已关闭 `RequestKind`、`RequestDescriptor::purpose`、capability mapping 与 `LlmCallKind` usage owner 的契约待办。保留 `RequestKind`，不新增 public `CallPurpose`；先前 ADR 0318、0319、0327 的阶段状态在路线图中标为历史切片。
2. **Phase 7**：ADR 0353 已修复 scheduled admission 清理谓词，使后续 admission 保留 Running row，供 completion/cancel/no-consumer recovery 路径使用。完整 Job lifecycle 仍未完成，继续记录 trigger/execution 分离、执行 timeout、claimant owner token/续租、dependency watcher durable/restart recovery 与跨 kind projection 等未决。
3. **Phase 8**：记录 ADR 0330、0335、0340、0341、0346、0347、0348、0350 已分别覆盖 session、action event/活跃 command、recording、Settings reads/hotkey、app event、agent event 与 session field mapping 域。此状态表示这些域的 mapper、validator 或审计边界已经收口，不表示有 Rust→TypeScript codegen。
4. 保留 ADR 0349 的结论：`SessionCompleted`/`SessionError` 与 `session:updated` 的跨 channel fan-out 没有共享 occurrence identity，不按 session/status 或相邻到达推断去重。
5. 保留 ADR 0351 的 Settings 未决项：compensation/rollback、失败后的显式 retry/restart recovery、Tools admin writers 与 Settings apply 的跨入口并发边界。其余未审计 command families、ask/input 与复杂 view/startup recovery 编排仍待后续切片。
6. 将 Phase 8 验收描述改为 CI 实际执行的 `check-ipc-contracts.ps1` 与 `check-ipc-events.ps1`，并保留 UI checks/tests/build 的适用门禁；不声称 CI 已有代码生成检查。当前 IPC registry 固定 71 个 Tauri commands，开发标准同步该数量。
7. 对已经被后续切片改变的旧快照增加“历史切片记录”说明，保留原切片事实和 ADR 关系；Phase 3 的 raw Database ownership 状态按 ADR 0299、0312 更新。
8. `AGENTS.md` 指向路线图当前阶段判断及对应 ADR 作为架构状态来源，并说明带日期的早期切片是历史快照；其中固定工程约束与执行规则不变。

## 影响与验证

只修改文档，不改生产代码、Rust/UI tests、配置、IPC、schema 或用户数据。路线图中的短 commit 引用均可由当前 HEAD 解析；ADR README 与本 ADR 链接已核对。

验证：执行文档内部链接与 heading anchor 检查、Markdown 格式/结构检查及 `git diff --check`。本次无代码变化，不运行 Rust 或 UI gates。

## 回滚

回滚本次文档提交即可恢复原路线图、架构说明、开发标准命令数和 ADR 索引；不需要数据或运行时迁移。
