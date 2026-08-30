# Git 提交流程

本项目采用“小步修改、可验证提交”的 Git 工作方式。提交是可审查、可回退的变更边界，不把未验证的工作留在主线上。

## 分支与历史

- `main` 只保留已经完成验证、可以作为下一轮工作的基线。
- 需要隔离开发时，默认使用 `codex/<short-topic>` 分支；没有明确隔离需求时，可继续使用当前工作分支。
- 不执行 `git push --force`、`reset --hard` 或改写共享分支历史；历史重置只允许作为明确授权的一次性迁移操作。
- 本地提交不等于发布，不自动推送远端；推送、合并和发布另行确认。

## 一轮改动的定义

一轮改动是围绕一个明确目标完成的一组相关修改，例如一次缺陷修复、一个稳定边界拆分或一组配套文档更新。无关的清理、实验代码和其他领域变更应拆成不同轮次。

在本项目中，Codex 完成一轮逻辑改动后，只要相关门禁通过，就主动创建提交，不等待再次确认。测试或检查失败时不提交，先修复或报告阻塞原因。

## 标准操作

### 1. 开始前确认范围

```powershell
git status --short
git diff --stat
git diff
```

确认当前分支、阶段目标和相关规范/ADR；保留用户已有修改，不覆盖、不代提交无关变更。

### 2. 完成后运行验证

代码、契约或配置变更至少运行相关测试；跨 crate 或跨端变更运行完整门禁：

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

同时运行适用的契约脚本、命名检查和 `git diff --check`。纯文档变更至少检查格式、链接和差异内容，不为没有代码影响的修改强行扩大测试范围。

### 3. 精确暂存

只暂存本轮需要提交的明确路径：

```powershell
git add -- <path1> <path2>
git diff --cached --check
git diff --cached --stat
git diff --cached
```

提交前确认暂存区没有密钥、令牌、用户数据、日志、构建产物、`target/`、`node_modules/` 或 `.svelte-kit/` 等生成内容。默认不使用 `git add .` 或 `git add -A` 扩大范围。

### 4. 创建提交并复核

```powershell
git commit -m "<type>(<scope>): <imperative summary>"
git status --short
git log -1 --oneline
```

提交成功后确认工作区只剩下有意保留的未提交修改；若本轮已全部完成，工作区应为空。提交钩子失败时不绕过钩子，先处理失败原因。

## 提交信息格式

格式为：

```text
<type>(<scope>): <imperative summary>
```

`scope` 可省略。常用类型：`feat` 功能、`fix` 修复、`refactor` 重构、`docs` 文档、`test` 测试、`build` 工具链/构建、`ci` CI、`chore` 维护、`perf` 性能、`revert` 回退。

标题使用动词开头、说明一个目的、不加句号，建议不超过 72 个字符。需要解释背景、契约变化、风险或验证方式时，在标题后追加正文。

示例：

```text
refactor(p2): complete P2 scope and hotspot cleanup
docs(workflow): define standard commit procedure
build(ui): pin Node.js toolchain
```

一次提交只表达一个可审查目的。提交完成后发现问题，默认创建新的修复提交；只有在提交尚未交付且用户明确要求时，才考虑 amend。

## 提交完成条件

- 变更范围和暂存内容经过复核。
- 适用的格式化、编译、静态检查、测试和构建均通过。
- 相关 ADR、架构文档、开发规范和用户可见说明已同步。
- 提交信息符合约定，提交后工作区状态符合预期。
