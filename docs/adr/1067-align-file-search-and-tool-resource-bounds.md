# ADR 1067：统一文件、搜索与模型工具资源边界

## 状态

已接受（2026-10-13）

## 背景

部分资源预算已有 Common 配置校验，但直接构造的运行时对象仍能绕过限制。文件读取、搜索结果、行范围搜索窗口、embedding 批次、provider 工具数组、模型上下文窗口、重试次数和剪贴板历史还存在配置范围与执行边界不一致的情况。搜索默认返回 50 条，也会忽略低于 50 的配置上限。

## 决定

- `context_limits.file_line_max_bytes` 范围为 1–128,000 字节；`file_byte_read_max_bytes` 范围为 1 字节–64 MiB，避免零字节上限生成互相矛盾的工具 schema 或触发运行时 clamp panic。`file_max_list_entries` 最多 10,000 条，0 仍返回空列表。
- `context_limits.search_max_results` 范围为 1–1,000。默认搜索结果数为 `min(50, 配置上限)`；单次请求不能超过配置上限。
- 单次搜索物化的 `results` 数组最多 4 MiB 序列化字节；文件名结果和内容结果（含上下文）共用预算，达到预算后停止并说明截断原因。根路径按 UTF-16 最多 32,767 个 code unit，模式最多 4,096 个 Unicode 字符，因此 envelope metadata 另有固定的小上限，不计入 4 MiB 数组预算。
- `context_limits.search_window_bytes` 最多 16 MiB；0 仍表示改走流式全文件扫描。
- `context_limits.embedding_chunk_size` 范围为 1–10。Common 提供者上限是 Agent 批次 clamp 的唯一来源。
- `context_limits.max_tools_per_request` 范围为 1–350，两个 deferred tool loader 和 catalog 均执行该范围。
- 默认与模型级 `context_window` 范围为 1,000–2,000,000 tokens；`response_token_floor` 不超过 2,000,000，压缩预留必须小于每个有效窗口。Router 对直接构造的配置执行相同归一化，并将 endpoint `max_tokens` 限制在有效上下文窗口内。
- 工具 `max_retries` 最多 10 次、retry backoff 最多 60 秒，工具 timeout 为 1–3,600 秒。执行器的重试 clamp 与 Common 声明上限一致。
- 剪贴板历史保留条数与单次查询条数分别最多 100；保留上限变更实时应用，0 清空并停止记录。每条进程内保留文本固定不超过 65,536 个字符；`clipboard_entry_max_chars` 单独限制 history 输出，最多 100,000 个字符。
- Common 校验 config 与 Settings；Settings parser/control 复用同一组界限。运行时构造器也 clamp 资源值，避免绕过配置入口的调用破坏上限。

## 影响

文件、搜索、模型请求和剪贴板资源预算在配置、Settings 和运行时一致。超过上限的 TOML 按既有策略备份整份配置并使用默认值；Settings 更新拒绝越界值。保留既有 0 值语义。配置 key、IPC shape 和数据库 schema 均未变化，无需重置数据库。

## 验证

- Common 测试覆盖配置与 Settings 的边界接受和越界拒绝。
- Agent/Tools 测试覆盖直接构造时的运行时 clamp，以及搜索默认结果数尊重配置上限。
- File search 测试覆盖序列化结果预算、请求路径/模式长度及截断提示。
- LLM/Tools 测试覆盖直接构造时的上下文、工具数、retry 和剪贴板保留/输出边界。
- UI contract 测试覆盖同一组数值边界，Settings 控件引用共享边界。
- 执行受影响 crate 的 check、Clippy、测试与 UI check/contract 测试；全量工作区门禁在本轮所有切片完成后统一执行。
