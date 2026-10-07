# ADR 0707：共用 Tool result 正文预览样式

## 状态

已采纳并实施。

## 背景

Agent、Clipboard、File、HTTP、JSON 与 Shell renderer 的 `.content-preview` 样式声明完全相同，分别重复颜色、代码字体、padding、wrap、180px 限高和滚动规则。Input renderer 使用同一视觉表面，但限高是 120px，且没有显式代码行高；Shell 流式预览将限高扩大到 280px。Window OCR 也使用旧 `.content-preview` class，却没有为其定义样式，因此该预览没有共享呈现。

## 决定

- 将预览元素统一命名为 `.tool-result-preview`，共享样式由 `ui/src/app.css` 唯一拥有。
- Window OCR 使用相同 canonical class，获得与其它 Tool result 正文一致的预览样式。
- 默认保持 180px 高度和代码行高；Input 用 `--tool-result-preview-max-height: 120px` 与 `--tool-result-preview-line-height: normal` 保留其布局语义。
- Shell 流式状态继续通过 `.tool-result-preview.streaming` 保留 280px 高度。

## 替代方案

- 每个 renderer 保留相同样式：拒绝。六处声明逐项相同，没有独立 layout owner。
- 将所有 preview 高度统一：拒绝。Input 的输入预览与 Shell live output 已有有意的紧凑/更大区域，审计不改变这些用户可见选择。

## 影响与验证

仅重命名内部 class 并合并视觉规则；文本内容、换行、滚动、默认高度与输入/流式 override 不变。Window OCR 现在复用统一预览呈现。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（690 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复各 renderer 的 `.content-preview` 声明与 class，并撤销全局样式、测试 selector、命名规范和路线图变更。无数据迁移。
