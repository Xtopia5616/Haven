# ADR 1183：增加 MCP Model-only 暴露策略

## 状态

已采纳并实现。

## 背景

ADR 1163 的 exposure 划分已区分原生调用、按需加载、CodeMode-only 和隐藏。进一步细分时，
需要一种策略让高风险或副作用较强的工具仍可作为显式顶层调用出现，但不能被 Workflow 或
CodeMode 等编排器隐式嵌套执行。Pi 的当前 extension exposure 同样将 model-only 工具与
CodeMode 可调用能力分开。

## 决定

- 增加序列化值 `model_only`。它与 `direct` 一样将 tool schema 放入 provider-native surface，
  顶层模型调用仍使用现有 ReAct、授权、确认、sandbox/network 和 MCP target-bound 执行链。
- `model_only` 不进入 Workflow capability 定义，也不允许通过 Workflow child resolver 或执行入口。
  该拒绝在 Workflow allowlist 构建时执行，并在每次子调用授权解析和每次执行 attempt 重查当前
  exposure；旧计划或配置竞态不能借用模型顶层调用资格。
- CodeMode 继续只接受显式设为 `codemode` 的 capability；`model_only` 不被 CodeMode discovery
  或子调用 resolver 接受。
- MCP 目录可以描述 `model_only` tool，以便模型读取精确 schema 和策略；目录发现不授予嵌套执行权。
- `direct` 保持现有含义，可进入 Legacy 原生 surface，也可作为受限 Workflow capability；
  `deferred` 仍必须按需加载后才成为 Legacy/Workflow capability。`hidden` 继续完全不可执行。
- 配置/UI 使用 wire value `model_only`；UI 明示其只允许顶层模型调用。默认值仍为 `deferred`。

## 影响

此策略只扩展 MCP exposure enum，不增加授权类别、执行器、数据库字段或新的权限 grant。Exposure
始终是路由约束，实际调用继续通过现有授权 proof、live policy 和 target-binding 检查。当前配置
消费者应将 `model_only` 视为新的闭合 enum 值；本项目不承诺旧版二进制读取新值。

正向与负向测试覆盖 provider-native 可见性、常规授权请求、Workflow surface 排除、旧 Workflow
计划的 live resolver 拒绝，以及 UI 配置保存。

## 参考

- [Pi Extensions](https://pi.dev/docs/latest/extensions)
- [ADR 1163：统一工具暴露、CodeMode 与受限工作流的执行边界](1163-tool-exposure-codemode-workflows-and-execution-boundaries.md)
