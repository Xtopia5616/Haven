# ADR 0826：说明空 UI Automation 控件结果

## 状态

Accepted — 2026-10-09

## 背景

`window.ui_tree` 与 `window.observe` 对某些桌面应用返回零个 UIA 控件。现有结果只报告 `count: 0`，容易让调用方误以为拿到了可用的控件树，也没有告诉用户下一步可以怎么做。

## 决定

- 当控件数组为空时，两个操作都返回固定 `note`，明确说 UI Automation 未发现可操作控件，并建议检查截图及使用坐标输入。
- 文案表达观测结果，不断言应用一定是 Qt 或一定缺少 UIA provider；空结果也可能由其它应用行为造成。
- Window renderer 展示 note。已有动态 JSON 输出契约和 validator 已允许根 `note` 字段，无需创建专用 IPC DTO。

## 替代方案

- 只返回 `count: 0`：拒绝。它没有解释能力边界或替代操作路径。
- 把目标应用标记为“不支持 UIA”：拒绝。空树不足以证明该结论。

## 影响与验证

仅在 UIA 返回空列表时增加 ToolResult `note`，非空树输出不变。适用门禁：Rust workspace 编译/严格 Clippy、前端类型检查与 `git diff --check`。测试因本次执行约束未运行。

## 回滚

删除空结果的 `note` 字段和 renderer 提示即可；无持久数据、配置或 IPC 重置。
