# ADR 0143：工具目录预算与高层交互能力

## 状态

Accepted — 2026-09-14

## 背景

工具目录已经从聚合工具迁移到 `root.operation`，但全局技能、session 工具和内置
operation 共同进入 provider 工具预算时，原有注册顺序截断会让可用能力不稳定。桌面
输入仍主要依赖屏幕坐标，连续文件修改也需要重复调用 `files.edit`。

## 决定

1. `list_defs_for_session` 使用稳定的来源优先级：核心 builtin、显式 session 工具、
   全局可选技能；每层按工具名排序，并在超限时记录省略工具和被省略的核心数量。
   MCP 显式加载仍使用原有的全有或全无预算准入。
2. UI 以 backend `ToolManifest` 为唯一的 renderer、label 和 root 来源；manifest 快照
   替换时清理已消失的条目，root 分组与 renderer 组件名保持分离。
3. 新增 `input.click_element` 与 `input.type_element`。它们通过 Windows UI Automation
   按窗口、控件名称、控件类型和可选索引重新定位目标；同名且未指定索引时拒绝执行，
   非 Windows 或不可用控件返回明确错误，仍使用原有安全网关和确认策略。
4. 新增 `files.patch`，对单个文本文件执行多个精确替换。所有匹配、重叠、大小、编码
   和取消检查完成后才进行一次写入；写回时保留 UTF-8 BOM、UTF-16 或 GBK 编码。

## 替代方案

- 继续按注册顺序截断：实现简单，但会让技能或 HashMap 顺序挤掉核心操作。
- 只增加更多坐标工具：调用参数短，但对窗口布局和分辨率变化脆弱。
- 让模型多次调用 `files.edit`：可复用现有接口，但中间状态更多且失败恢复更困难。

## 影响与验证

工具名称、权限 key 和现有坐标输入保持兼容；新增操作按普通内置 operation 进入
schema、manifest 和安全矩阵。UIA 控件动作仍是 Medium 风险，文件 patch 仍是 Medium
风险。验证包括：

```text
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
cd ui && corepack pnpm run check
cd ui && corepack pnpm run test:run
```

回退时删除新增 operation、恢复旧目录选择与前端 fallback 即可，不涉及数据库或快照
迁移。
