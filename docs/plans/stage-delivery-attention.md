# 阶段交付后等待用户确认

## 目标与边界

已结案的 `1c9af6b0-827e-4dfa-a752-946c68f7ee8d` 中，持续目标同时把报告回执写成完成条件，又要求交付后等待用户审阅。协调 Agent 上报后直接结案。仅修正用户明确要求当前 Issue 交付计划、报告或附件后留在原 Issue 等待决定的指引。报告回执不是整个 Issue 的完成条件。无明确等待要求时，Owner 仍可按原目标判断是否结案。不改变状态机、协议或数据，不重开原 Issue，不部署。

## 注入路径

- Router：`crates/tend-server/src/router.rs` 的 `ROUTER_INSTRUCTIONS` 经 `Client::start_thread(...)` 传到 `crates/codex-app/src/lib.rs` 的 `thread/start` 请求 `developerInstructions`。递增 `TOOL_REVISION`，让下一次 Router 执行使用新 thread。派发的持续 `objective` 保留审阅步骤，Router 在原 Chat 回报并发布相关 Attention，派发后仍结束当前回合。
- Issue Agent：`docs/roles/issue-agent.md` 经 `crates/tend-runner/src/tools.rs` 的 `catalogue("issue-router")` 中的 `include_str!` 载入。`crates/tend-runner/src/execution.rs` 通过 `app.start_thread(&cwd, instructions, &tools, ...)` 将该指引传入同一个 `thread/start` 请求的 `developerInstructions`。Issue Agent 上报实际结果和原件、登记一个可作答的 Attention，结束当前 Run 让出 slot，保持 Issue 开放，收到答复后再决定后续。

## 定向验证

在两个 App Server fixture 的 `thread/start` 边界各断言一条新指引，Runner 只针对 Issue Agent 验证。以固定回调增加交付后审阅场景，用一个 slot 完成子项交付，顶层上报并登记确认选项。确认报告和可下载原件、Attention 都回到原 Chat 并引用原消息。答复前核对 Issue 开放、原 Run 已停妥、原分配处于等待且无活跃 Run。随后在原 Chat 答复并核对负责顶层 Issue 的 Agent 在原 Task 的新 Run 收到 `attention_answered`，然后才结案。用显式 fixture 门控和持久事实控制顺序，不用固定延时猜测状态。

用仓库工具链运行锁定构建和格式检查，跑阶段验收以及 `attention.py`、`delivery.py`、`coordination.py`、`chat.py` 的相关回归。固定 App Server 验证注入和接续机制，不能证明真实模型会选择同样的工具调用。真实 Codex 验收只在独立已认证 home 与隔离仓库可用时进行。
