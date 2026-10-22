# ADR 1369：移除生成采样参数配置

- 日期：2026-10-22
- 状态：已接受

## 背景

生成采样参数极少使用，且不同模型和推理模式支持不一致。Provider 默认温度、模型覆盖和协议兼容分支增加配置与维护成本。

## 决定

删除 temperature、top_p、top_k、frequency_penalty、presence_penalty、seed 的生成配置、默认值、物化、校验、协议字段、请求赋值、UI 和能力提示。所有生成请求依赖服务端默认采样行为，不再维护按模型或推理状态省略字段的兼容层；智谱搜索请求中的硬编码采样值也删除。

保留 token 上限、推理强度、停止序列和结构化输出等独立执行控制。独立搜索 top_k 表示结果数，不属于生成采样。

官方参考：[OpenAI reasoning](https://developers.openai.com/api/docs/guides/reasoning)、[Claude thinking](https://platform.claude.com/docs/en/build-with-claude/thinking)。这不是所有供应商统一废弃采样字段的结论，而是 Haven 主动缩小公开配置契约。

## 替代方案

保留高级入口或扩展自动能力探测会继续承担兼容矩阵成本，因此不采用。

## 影响、安全与重置

旧 Provider/model/endpoint 采样键直接由 deny_unknown_fields 拒绝，不提供迁移或旧字段别名。按 release-and-reset.md 删除相应配置行或重建 config.toml；数据库、凭据和会话不变。请求不增加外部能力、敏感字段或权限；生成随机性由供应商默认控制，可能与旧温度设置产生不同输出。

## 验证与回滚

四个生成协议的请求 fixture 与负向断言验证不发送采样字段；Common 测试验证旧字段拒绝。执行 workspace 格式、编译、Clippy、测试，UI 检查、测试、构建及 IPC 漂移检查。回滚通过撤销本提交恢复代码；如需原采样设置，使用升级前自行备份的配置。

验收结果：workspace check、严格 Clippy、格式检查、完整 Rust 测试（含 doc tests）、UI check、1395 项前端测试与生产构建、IPC 漂移检查、ADR 索引和依赖边界检查均通过。首次全量测试的既有 CodeMode 取消测试出现时序失败，单独复测及完整重跑均通过。D 盘容量耗尽时改用 C 盘独立 target，关闭 incremental/debug symbols 完成 Rust 验证。
