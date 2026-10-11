# ADR 0988：统一配置与 Settings 的语义校验

## 状态

已采纳并实施。

## 背景

配置载入时会拒绝未知 API 风格和悬空媒体 Provider 引用，但 `ConfigLoader::save` 与 `ConfigService::edit` 没有复用这些检查；Settings IPC 又单独维护模型 Provider 检查。结果是一个设置可能被保存成功，却在下次启动时触发配置备份并回退默认值。数值校验也只覆盖 JavaScript 可往返性和部分浮点范围，没有统一 MCP 定时器及录音缓冲的资源边界。

## 决定

1. `haven_common::config::validate_app_config` 是配置语义不变量的唯一校验入口，覆盖数值契约、MCP profile 名称、API 风格、媒体 Provider 引用、MCP 发现/重连间隔，以及受限资源上限。`validate_settings_contract` 另外要求 Settings 最终模型均绑定已配置 Provider；路由/发现期间的未分配 ModelConfig 仍可存在于 AppConfig。
2. 配置 load、save、ConfigService 构造与 edit 均调用同一入口。ConfigService 在安全凭据持久化之前验证 edit 候选值；失败时恢复旧快照。
3. `validate_settings_contract` 将 Settings 投影为 AppConfig 后复用同一语义校验，并集中检查凭据必须通过 staging 命令写入、MCP 环境变量必须通过 MCP 设置命令变更。Settings 和 AppConfig 的投影由共享字段宏生成，避免字段列表漂移。
4. MCP 健康检查和重连初始/最大间隔必须大于零，最大重连间隔不得小于初始间隔；重试次数可以为零。`input_ring_buffer_secs` 允许零（使用捕获默认值），最大为 120 秒，按 mono 16 kHz `f32` 估算最多约 7.7 MB。
5. UI Settings 边界拒绝负整数、小数整数、超过 Rust `u32`/`u16` 域的值，以及已定义浮点范围外的值；并同步检查 MCP 间隔和录音缓冲上限。Common 仍是最终校验者。
6. 不改变配置字段名、IPC wire shape 或数据库。旧 TOML 若含新规则拒绝的值，继续沿既有 timestamped backup 与默认配置流程处理；无需数据库迁移。

## 影响与验证

- Rust 覆盖配置与 Settings 共享语义规则、保存时拒绝无效 API 风格、无效配置值失败不改文件、MCP 间隔和录音缓冲边界。
- UI 覆盖负数、小数、`u32`/`u16` 越界、浮点范围和零/倒置 MCP 间隔。
- 验证：`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引检查。

## 回滚

恢复到统一校验前会重新允许无效配置经编辑写盘，并允许配置值触发无界环形缓冲或零间隔轮询。无需数据库重置；配置恢复沿项目既有备份流程进行。
