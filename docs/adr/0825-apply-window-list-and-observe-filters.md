# ADR 0825：严格应用窗口列表与观察筛选器

## 状态

Accepted — 2026-10-09

## 背景

`window.list` 接受标题/condition 输入，但实现只按 PID 过滤；`observe` 和 `ui_tree` 将 PID 丢给 UI Automation，导致传入某个进程 PID 时实际可能观察前台的 Haven 窗口。使用 `window_id` 时也没有验证附加 PID/标题条件是否与该 HWND 对应，容易将自动化结果归到错误窗口。

## 决定

- `window.list` 对 `title` 做不区分大小写的子串筛选，并与 PID 条件取交集。`condition` 在 list 上只接受 `title_contains`；`foreground_contains` 和 `ui_text` 只用于 wait。
- `ui_tree` 与 `observe` 将 HWND、标题和 PID 合并成同一窗口选择器。存在多个 selector 时必须全部匹配；PID 单独匹配到多个顶层窗口时返回歧义错误。没有显式 selector 时才沿用前台窗口默认值。
- PID 输入限制为正的 32 位 Windows PID 值，防止 `i64` 到 `u32` 转换截断后匹配到另一进程。

## 替代方案

- 忽略多余 selector 并回退到前台窗口：拒绝。它会将输入送入或读取错误应用。
- PID 对应多个窗口时选第一个：拒绝。枚举顺序不是稳定的用户意图。

## 影响与验证

Window 输入 schema 补充 list 标题筛选与 ui_tree PID 字段；ToolResult 输出结构不变。失败选择器明确返回错误，没有数据或配置变化。适用门禁：`cargo fmt --all -- --check`、Rust workspace 编译与严格 Clippy。Windows 窗口枚举及 UI Automation 的真实桌面行为仍需 Windows 会话验收；测试因本次执行约束未运行。

## 回滚

恢复 list 仅按 PID 筛选、恢复 UI Automation 忽略 PID/附加 selector 的行为。仅影响进程内窗口选择，不需要数据或配置重置。
