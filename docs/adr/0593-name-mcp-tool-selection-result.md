# ADR 0593：为 MCP tool selection 使用具名结果

## 状态

已采纳并实施。

## 背景

Tools 的 `select_tools` 按请求名称筛选 MCP server 暴露的工具，同时保留两种不同顺序：选中工具跟随 server 返回顺序，缺失名称跟随用户请求顺序。函数通过 `(selected, missing)` 返回，两者随后进入 tool-budget 判断、激活与结果 JSON。

## 决定

1. 以 `McpToolSelection { selected_tools, missing_tool_names }` 表达筛选结果。
2. `select_tools` 对无过滤请求仍返回全部 server 工具与空缺失列表。
3. selected server order、missing request order、budget gate 和结果字段保持不变。

## 替代方案

- 保留 tuple 并依靠变量名解构：拒绝，函数与测试仍依赖位置；后续字段扩展也更易错配。
- 在筛选时把缺失名合并进工具清单：拒绝，工具定义与用户请求诊断属于不同值域和用途。

## 影响与验证

- 改动限于 `haven-tools` 私有 MCP 加载结果，不改变 Tool JSON、IPC、事件、注册行为或预算判断。
- 更新命名路线图；无需持久数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、`cargo test --locked -p haven-tools`（799 unit + 7 integration passed / 2 ignored）、ADR 索引与 `git diff --check`。

## 回滚

恢复 `(Vec<McpToolInfo>, Vec<String>)` 返回并还原 tuple 解构；不涉及持久数据。
