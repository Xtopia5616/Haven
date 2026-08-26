# ADR 0007：设置诊断命令使用命名响应 DTO

日期：2026-08-26
状态：已采纳

## 背景

设置页的 `get_log_info` 和 `check_shell_available` 直接返回 `serde_json::Value`。这两个响应
虽然字段很小，却没有固定的 Rust 类型，也没有前端边界校验；后续字段漂移会在页面深处静默
表现为 `undefined`。`read_log_tail` 已有命名的 `LogTail` 响应，但前端仍未统一验证；同一
设置页的 `get_api_key_status` 也用动态对象表达固定的状态字段，无法清楚区分 provider 名称
这一扩展点和其它稳定字段。

## 决定

- `get_log_info` 返回 `LogInfo { enabled, level, path }`。
- `read_log_tail` 继续返回 `LogTail { path, content }`，并由前端统一解析。
- `check_shell_available` 返回 `ShellAvailability { available }`。
- `get_api_key_status` 返回命名的 `ApiKeyStatus`，固定模型/媒体状态字段，只有 provider 名称
  保留为 `Record<String, bool>` 扩展点。
- 前端在 `contracts/settings.ts` 验证四种响应；校验失败进入既有错误处理，不继续消费未知形状。
- 不向响应加入日志目录、PATH、探测命令或其它本机环境细节。

## 替代方案

继续从 `serde_json::Value` 读取，或在每个 Svelte 页面各自做字段检查。前者无法在编译期表达
契约，后者会产生多个边界和不一致的失败语义，均被拒绝。

## 影响

命令成功响应的 JSON 字段保持不变，因此不影响已有用户配置或日志文件。畸形的 renderer/后端
响应现在会被前端拒绝并进入错误提示路径。没有新增持久化数据，不需要重置。

## 验证

Rust 单测固定 `LogInfo`、`ShellAvailability` 与 `ApiKeyStatus` 的 wire shape；前端测试覆盖
四种响应的正向解析、`null` 日志路径和错误类型；执行 Rust workspace 测试、严格 Clippy、Svelte check、
Vitest 与生产构建。

## 回滚

可回滚命令返回类型和前端解析器，但不能在同一版本保留两套响应形状。回滚不涉及数据库、配置
或缓存。
