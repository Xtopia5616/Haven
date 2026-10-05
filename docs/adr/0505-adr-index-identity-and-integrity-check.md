# ADR 0505：固定 ADR 编号身份并校验目录完整性

## 状态

已采纳并实施（2026-10-05）。

## 背景

`docs/adr/README.md` 规定 ADR 文件名使用 `NNNN-slug.md`，编号递增且不重用。目录审计发现 8 组重复编号，共 487 篇 ADR 中有 22 篇未列入索引；直接链接只覆盖其中一侧，因此普通 README 检查不会发现编号冲突或遗漏。仓库原有 CI 也没有验证 ADR 链接。

ADR 编号是跨文档引用身份的一部分。对已有决定重新编号会使同号正文引用产生歧义，因此需要以版本历史确定保留者、按单一递增序列分配新号，并避免对裸数字做全局替换。

## 决定

1. 每篇 ADR 必须有唯一的四位编号；每个 README 索引目标必须与文件名编号一致，且每篇文件恰好出现一次，索引按编号递增。
2. 每组冲突保留最早进入 Git 历史的 ADR 编号。较晚引入的 ADR 按其首次提交时间依序使用当前最高编号 0496 之后的新编号；已占用编号不回填，不重用：

   | 原文件 | 新文件 |
   |---|---|
   | `0169-provider-adapter-module-layout.md` | `0497-provider-adapter-module-layout.md` |
   | `0173-recovery-and-provider-failure-boundaries.md` | `0498-recovery-and-provider-failure-boundaries.md` |
   | `0187-capability-policy-canonicalization.md` | `0499-capability-policy-canonicalization.md` |
   | `0185-session-termination-reasons.md` | `0500-session-termination-reasons.md` |
   | `0154-network-ask-default.md` | `0501-network-ask-default.md` |
   | `0196-session-actor-event-sourced-state.md` | `0502-session-actor-event-sourced-state.md` |
   | `0405-window-resize-aspect-ratio-bounds.md` | `0503-window-resize-aspect-ratio-bounds.md` |
   | `0406-width-based-adaptive-layout-breakpoints.md` | `0504-width-based-adaptive-layout-breakpoints.md` |

3. 更新指向被重新编号 ADR 的明确 Markdown 文件链接和已核对的正文引用。裸编号按语义逐项判断；相同编号但指向不同 ADR 的正文（例如 UI 的 ADR 0196，以及配置契约的 ADR 0405）保持不变。
4. 将遗漏的 ADR 全部加入 `docs/adr/README.md`。`scripts/check-adr-index.ps1` 校验文件名格式与唯一性、README 精确覆盖和升序、编号与链接目标匹配，以及文档中的本地 ADR 链接；CI 的 rust-check job 执行该脚本。
5. 仅变更文档编号和链接，不修改 ADR 所记录的架构决定、源代码行为、配置、IPC、数据库或用户数据；无需迁移或重置。

## 替代方案

- 只在 README 隐藏重复项：拒绝。文件名和外部引用仍然含糊，索引仍无法做到精确覆盖。
- 对所有文本全局替换旧编号：拒绝。重复编号已经指向不同主题，UI reducer ADR 0196 与 SessionActor ADR 0196、配置 ADR 0405 与窗口比例 ADR 0405 都存在独立引用。
- 重编号所有冲突文档：拒绝。最早入库的稳定身份无须改变；只为后出现的冲突分配新号，变更范围更小。
- 把新编号放回 0487–0494：拒绝。编号按历史规则不重用，当前最高号为 0496，应继续递增。

## 影响与验证

目录中的 487 篇既有 ADR 编号唯一，README 与文件集合一一对应；历史决策正文只在明确指向被重编号文档时更新链接。新增 PowerShell 检查在 CI 运行，未来重复编号、漏列、错链、顺序漂移和失效本地 ADR 链接会阻断检查。

验证命令：`pwsh -NoProfile -File scripts/check-adr-index.ps1`，并运行 `git diff --check`。该切片为文档与 CI 校验变更，不触及 Rust/UI 行为。

## 回滚

可回退 CI 步骤、检查脚本、索引和 8 个文件名及引用的对应改动；这只会恢复文档歧义，不涉及任何运行时或持久化数据。编号一旦分配后不应在后续新 ADR 中复用。
