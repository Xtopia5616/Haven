# ADR 0868：按文本截断输出形状命名 helper

## 状态

Accepted — 2026-10-10

## 背景

`haven-common::error` 与 Agent `prompt.rs` 都有私有 `truncate_chars`，但输出契约不同：Common 错误摘要截断后追加省略号，Agent prompt budget 只取前 N 个字符以满足布局预算。Agent 还声明 `sanitize_prompt_field`，实际固定调用 Common sanitizer 并设置 256 字符上限，仅用于 memory fact prompt；Common 同名函数则接受调用方传入的任意字符预算。

## 决定

- Common 错误摘要内部 helper 改名为 `truncate_with_ellipsis`。
- Agent prompt helper 改名为 `take_prefix_chars`，准确表达按字符数截取前缀、不追加标记的输出。
- Agent 的固定 256 字符事实 prompt sanitizer 改名为 `sanitize_fact_prompt_field`；Common 通用 sanitizer 保持 `sanitize_prompt_field(input, max_chars)`。
- 保持所有预算值、清洗顺序、换行/ellipsis 策略、返回文本和 token accounting 不变；这些 helper 继续由各自 owner 实现，不合并不同输出契约。

## 替代方案

- 合并两个 `truncate_chars`：拒绝。Common 错误文本要显示省略号，Agent prompt prefix 必须严格服从布局预算且不能额外追加字符。
- 删除 Agent 的固定预算 helper、在每个调用点重复 `256`：拒绝。256 是 fact prompt projection 的统一领域上限，应保留单一 policy name。
- 给 Common `sanitize_prompt_field` 增加默认预算或 Agent 专属参数：拒绝。Common helper 只应用调用方提供的长度，不拥有 Agent prompt 的产品预算。

## 影响与验证

重复名称现显式区分 output shape 与领域预算；没有行为变化或 API/wire/数据库变化。workspace、测试目标编译及严格 Clippy 通过；测试套件未执行。

## 回滚

若新增文本转换，应按截断是否添加标记和长度预算的 owner 命名，不恢复含糊的跨领域同名 helper。当前无持久数据需要重置。
