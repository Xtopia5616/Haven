# ADR 0829：为新建 Skill 提供 JSON 安全的默认脚手架

## 状态

Accepted — 2026-10-09

## 背景

`skill_create` 原先要求调用方从零提供 Python 脚本。skill runner 以 JSON stdin/stdout 作为协议，脚本中的普通 `print()`、本地代码页解码或错误分支输出纯文本都会破坏结果解析；用户提供的脚本因此需要重复处理协议细节。

## 决定

- `script` 改为可选；省略时写入内置 Python starter template。非空限制与最大脚本字节数仍适用于显式脚本。
- 模板通过 `sys.stdin.buffer.read()` 读取输入并按 `utf-8-sig` 解码；所有成功和 Python 异常路径生成一个 JSON envelope，stderr 保持为空。
- 模板在运行 `run(params)` 时将 Python stdout 重定向至内存缓冲，避免调试 `print()` 混入协议输出。
- 建议 `run(params)` 返回 `hashes`、`detected` 等结构化字段及面向人的 `report`；额外字段放入 envelope 的 `data`。

## 替代方案

- 继续要求每个调用方从零编写 I/O 协议：拒绝。它重复暴露编码与 stdout 解析错误。
- 静默清洗任意自定义脚本的 stdout/stderr：拒绝。Haven 无法可靠地区分协议结果与任意脚本输出；自定义脚本仍必须遵守 runner 的 JSON/stdout 契约。

## 影响与验证

工具输入 schema 仍接受显式 `script`，但不再将其列为必填项；操作仍创建可执行 skill，并保留既有风险分级与授权策略。没有 IPC 输出、配置持久化格式或数据库变化。适用门禁：Rust workspace 编译、严格 Clippy、格式和 diff 检查。测试因本次执行约束未运行。

## 回滚

恢复 `script` 必填及旧提示即可；新建 skill 的脚本仍是普通文件，不需要配置或数据库重置。
