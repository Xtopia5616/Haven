# ADR 0609：命名 LLM model pricing 结果

## 状态

已采纳并实施。

## 背景

`extract_pricing` 解析 OpenAI-compatible model discovery 行，并把 provider 的 prompt/input 与 completion/output 单 token 价格换算为每千 token 的美元价格。原返回 `(Option<f64>, Option<f64>)`，唯一生产调用方立即把它们绑定为短名，再填入 `ModelInfo` 的 input/output 价格字段。

## 决定

1. 用内部 `ModelPricing` 结构携带 `cost_per_1k_input_tokens` 和 `cost_per_1k_output_tokens`。
2. `model_info_from_json` 按字段映射到对应的 `ModelInfo` 字段。
3. 继续分别解析 input 与 output 价格；缺失或非法值仍独立表示为 `None`。

## 替代方案

- 保留 tuple，只把局部绑定改成长名称：拒绝，函数结果仍由顺序而非领域字段定义。
- 将两个方向合并成一个单价：拒绝，provider 对输入与输出分别报价，模型成本也分别消费这两个值。

## 影响与验证

- 仅改变 `haven-llm` 内部 model metadata parser 的结果类型；provider 字段识别、单位换算、缺省和 `ModelInfo` 序列化不变。
- 命名路线图 §5.7 继续保持 Active；其他 crate 的函数、类型 owner 与跨层契约仍待审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、`cargo test --locked -p haven-llm`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(Option<f64>, Option<f64>)` 返回值和 tuple 解构；无需配置、数据库或 provider wire 迁移。
