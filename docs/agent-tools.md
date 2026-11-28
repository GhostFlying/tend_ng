# Agent 工具与本地受理

一期采用 **Skill + 本机注入 Tend 工具**。Agent 以非沙箱模式运行。Router 的工具由本机 Server 处理，Issue Agent、Work Agent 和 Review Agent 的工具由运行它们的本机 Runner 处理。需要代码的工作使用上层派发所分配的目录。Skill 按角色说明工具、参数、回执和建议的工作方式。Agent 结合项目的实际要求决定步骤。用户 CLI 覆盖全部公开能力，与 Web 使用同一套 Server API。Agent 调用与用户 API 共用业务处理、权限校验和回执，各自携带实际调用身份。

## 调用接口

- **调用和身份。** 注入工具声明可执行的业务动作与参数。运行 Agent 的实例根据原生调用和持久化的执行关联确认角色及责任归属，Server 应用时校验实际来源及目标。用户 CLI 提供帮助和结构化输出。
- **短 Skill。** Router 读取相关工作、派发顶层 Issue，并代理主 Chat 的答复、回报和 Attention。Issue Agent 委派工作子 Issue，转交原输入、跟进交付和结案，并向 Router 上报。Work Agent 记录工作事实、提交产物、结算自己的 Issue，对 PR / CI 使用带明确条件的 `subscribe` 或 `waitfor`。顶层直接 Owner 同时可上报本 Issue 和直接子项交付、提出 Attention 和按需协调子项。 [直接 Owner](roles/direct-owner.md)Review Agent 提交所审范围和发现。Skill 讲清怎样操作及怎样拿到回执。Runner 随原生回合载入对应的短指引。 [Issue Agent](roles/issue-agent.md) · [Work Agent](roles/work-agent.md) · [Review Agent](roles/review-agent.md)
- **工作目录。** Issue Agent 在拆分时按需为子 Issue 指定独立 worktree。Work Agent 使用分配的目录，如需独立 worktree 先向上级提出，日常 Git 操作遵循项目约定。 [Workspace 分配](server-runner.md#workspace-分配)
- **PR 关联。** Agent 可以登记本次工作的 GitHub PR 或 Codebase MR URL，也可以引用已有 Task 或 worktree 发起分支补查。两种操作都沿目标 Issue 保存关联，并沿原操作 ID 查询结果。 [PR 与 Issue 关联](design.md#pr-与-issue-关联)
- **信息来源。** Router 在 Server 本地查询 Issue 及其持久关联记录。Runner 上的 Agent 断线时可读随输入带来的工作快照和本地回执。若需要最新的 Server 事实，查询可以异步提交，并按原请求读取结果。工具结果标明事实的来源和待同步状态。
- **逐级上报。** Agent 可以沿 Issue 的委派关系上报具体问题，附原 Issue、已有证据和影响。Issue Agent 用 `tend_answer_child_attention` 回答权限内的子问题。确需用户决定时，用 `tend_escalate_attention` 传原 `attention_id`，原问题保持未答，Router 沿原 ID 发布。只有 Issue Agent 自己的新问题才用 `tend_request_attention`。顶层交付仍由 Router 在原 Chat 回报。 [逐级上报与用户决策](chat-routing.md#逐级上报与用户决策)
- **取消转达。** Agent 依据原取消范围向自己委派的子项传达取消，使用工具向明确的 Task 提交停止控制，并按原操作查询受理、送达及实际停妥证据。 [取消与逐级停止](task-lifecycle.md#取消与逐级停止)
- **审阅记录。** Work Agent 在当前工作子 Issue 登记原生 subagent 审阅的候选、范围和结论，并关联 Runner 观察到的子 thread 和结果。Runner 依据 App Server 的原生终态或历史确认执行状态。 [独立审阅](task-lifecycle.md#独立审阅)

收到 `attention_answered` 不等于用户亲答。Work Agent 只有在输入中有 `user_message_id`、`user_text` 等直接证据时才将决定归因给用户，否则只报告决定本身。

`tend_request_review` 由原作者提交非空候选和范围。Git 审阅只接受作者已分配 repo 的完整提交 SHA（`base_refs`），不把原目录的 HEAD 当作候选。提交文件可用 `artifact:<file_id>` 引用作者 Task 的不可变交付副本，Reviewer 从持久回合上下文的 `bound_review_artifact` 读取。作者取得回执后可以继续收尾，实际停妥后 Reviewer 才能取得同一目录。

收到结果后如需复审，作者提交新候选并传入结果的 `child_task_id` 作为 `review_task_id`，不传 `agent_id`。该输入进入原 Review Issue、Task 和 Reviewer。Reviewer 用 `tend_deliver_review` 回传结论和证据，Runner 从当前 Run 填入确切的请求身份。重复调用沿原回执对账，只有本轮第一条结果产生作者输入。普通讨论或结案不替代结构化证据，用户授权也不走该工具。

经过同机 Runner 接替的 Assignment 可附 `review_history`，供新原生会话识别先前候选和结果。历史不是新的审阅结果或本轮输入。接替不授权跳过原审阅停妥检查，也不自动发起复审。

派发方可以直接提交工作要求，也可以用本机工具查询合格候选再指名。候选信息来自已同步的清单，Server 应用时按当前事实确认。 [具名 Agent 与任务分配](agent-selection.md)

## 角色 Skill 与仓库约定

旧 Tend 的 Git 后端从多仓库共用的外层目录启动，Runner 生成一份 `AGENTS.md` 来说明目录、起始提交和各仓库的指令位置。Orca 路径直接使用实际仓库的 worktree，把 Tend 的说明放进任务简报。 [旧外层目录指引](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/agents.go#L12-L41) · [旧仓库简报](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/runner/gitws/agents.go#L44-L59)

**已确认。** Work Agent 从分配的 workspace 根启动。Runner 在根准备外层 `AGENTS.md`，记录各仓库的挂载目录和起始提交，指引 Agent 读取需要操作的仓库自己的 `AGENTS.md`。本轮目标、范围及预期交付由原 Task 输入提供，角色指引和 Tend 工具用法由对应 Skill 提供。 [多仓目录布局](server-runner.md#多仓目录布局)

复用首个切片的真实 App Server 验收，在示例仓库放一条可核对的工作约定。验证 Agent 从 workspace 根启动，能沿外层指引读取仓库约定，也能读取对应角色 Skill。注入的 Tend 工具按实际 Agent 身份受理。 [首个产品切片](plans/first-product-slice.md#最少验收)

## Codex 注入 Tend 工具

**已确认。** 受管 Agent 调用 Codex App Server 注入的 Tend 业务工具，例如派发、留言、交付和 `waitfor`。工具描述单项业务动作，Agent 根据 Skill 和工作事实决定怎样组合使用。

App Server 以 `item/tool/call` 交给执行端，回调携带 `threadId`、`turnId` 和 `callId`。执行端依据持久的回合关联确认发起方。Server 找到原 Router 输入，Runner 找到原 Issue、Task 和 Run。工具参数描述动作和目标。复用 thread 后的工具调用按它自己的 `turnId` 归属。启动响应丢失时，沿此前保存的原生尝试和可查事件找回原关联。 [Router 输入的恢复边界](chat-routing.md#router-输入的恢复边界)

本机受理时保存原生调用和业务操作 ID 的关联。状态、回执及待投递记录由所属实例在短事务中持久提交，再给 App Server 返回工具结果。重复递送同一原生调用沿原操作查证。Router 的调用在本机 Server 结算，工作 Agent 的调用由本机 Runner 受理、再异步同步。工具返回的回执显示当前已到的阶段。 [本地受理与业务结算](#本地受理与业务结算)

旧 Tend 按原生调用身份找到回合和责任，部分 Runner 工具跨实例发出事件后等待回包。新版沿用回调携带的执行身份，把工作 Agent 的回执定在 Runner 本地持久提交。已用实际 App Server 验证同一 thread 内的新回合归因、启动响应丢失时的关联，以及重新加载原 thread 后的工具调用。 [旧工具调用身份](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/mainthread/mainthread.go#L1096-L1135) · [旧跨实例回调](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/harness/coordination/session.go#L834-L877) · [动态工具实测](plans/rust-offline-smoke.md#codex-动态工具的执行身份验证)

## 本地受理与业务结算

1. **本地已受理。** 工具由运行 Agent 的实例处理。本机完成持久受理后返回可查证的操作 ID。Runner 受理时保存原意图和待发送记录，Server 离线时保持待同步。Router 的请求在 Server 本地受理，可以从同一短事务返回已提交的业务结果。
2. **Server 已结算。** Server 按原操作 ID 去重，应用业务变化并保存回执。主 Chat 的派发或已有 Issue 的补充入队与可见受理消息共用事务和重试身份。受理不表示实际领取。异步匹配和领取沿持久派发记录推进。

`tend_publish_progress` 是 Router 对主 Chat 当前直接用户输入发布完整进展消息的工具。它按原 Router 输入保存首条进展，返回已发布的消息 ID。同一工具重复调用或原输入因丢失回合而重放时，返回同一 ID，不改写已发布内容。它不用于后台回报或 Discussion 转交，不等待本轮最终输出。
3. **原操作可查。** 用户 API 重传沿原操作 ID，App Server 重递送原生调用时按持久关联读回原操作。新 session 可以读取相同的输入和回执。执行历史用于追踪原调用，业务操作身份在整个生命周期内保持稳定。

## 操作身份及首次回执丢失

**已确认。** 用户操作 ID 在第一次发出请求前由 Web 或 CLI 确定。Web 在发送时保留原 ID 和对应输入。CLI 默认管理 ID，也允许调用方显式提供 ID。CLI 内部的请求重试沿用同一 ID，超时时返回原 ID 供后续查询。注入工具的操作 ID 由执行端关联到原生调用，工具回执供 Agent 查询。

同一发起方沿原 ID 重传相同动作、目标和参数，读取已提交的收件或结算结果。原 ID 带来不同意图时报告冲突，保留原记录。消息 ID、Issue ID 和回执由拥有其写入权的实例在受理或应用时关联到原操作。Runner 将同一操作 ID 带到 Server，Server 按实际来源校验后应用。CLI 进程或 Agent session 丢失原 ID 时，可以沿原输入和 Issue 查当前权限范围内的已受理操作。查询时接收结果仍无法判定的操作保留待查状态。

旧 Tend 已为逻辑请求固定 `request_id`，Web 发消息前会生成并保存到发送记录。#393 记录了负责人事件已被处理、SQLite ACK 写入超时后原执行中止的情况。新版收件与推进所需记录一起提交，各入站操作的回执从已提交记录生成，故障恢复沿原身份读取。 [旧请求身份](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/protocol/envelope.go#L16-L32) · [旧 Web 发送记录](https://github.com/GhostFlying/tend/blob/83bc15ae/web/src/queue/sendQueue.ts#L182-L217) · [负责人 ACK 恢复](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/manager-run-timeout-recovery.md#L1-L8) · [首个切片的定向验收](plans/first-product-slice.md#最少验收)

`tend_propose_approval` 由支持该能力的 Server 开启。顶层 Issue Owner 可以引用具体的正式报告操作和 Markdown 原件登记待批准版本。修订以原 Attention ID 和下一个版本提交，不登记第二个等待义务。回执仍是 Runner 本地受理。 [结构化方案批准](plans/inline-proposal-approval.md#implemented-contract)

`tend_rename_issue` 只接受新标题。原生回调关联到正在执行的已领取 Run 后，Runner 将当前 Issue 和原操作一起持久受理。已保存的同原生调用返回原回执，其他角色不能传入目标或操作者。回执不是 Server 已改名的证明。 [Issue 标题契约](task-lifecycle.md#issue-标题与改名历史)

## Session 接续

所有角色的原生会话都在所属服务或 launch profile 显式配置的独立 Codex home 落盘，包括受限研判与交接。启动或恢复均不使用个人默认 home。 [排障与原件归属](codex-rollouts.md)

**已确认。** 新 session 收到本轮输入和接续所需的工作摘要。摘要从持久的 Chat、Issue、Task 和操作记录读取。Agent 通过工具把会影响后续行动的事实写入 Issue。同一 Task 的兼容且已停妥的原生 thread 优先续用，默认只补本轮输入和相关变化。缺失或已知不兼容时，新 session 带原工作契约、当前输入与近期已持久化原输入和受理回执。线程是执行上下文，不能代替 Issue、Task 或业务回执。

- Router 从原 Chat 输入和相关工作摘要接续，可以查询该输入已提交的派发和回复。
- Issue Agent 的摘要可额外带上子 Issue 的当前状态、交付概况和待协调事项。具体子项按引用查询。
- Work Agent 从原 Task、已收补充和原 workspace 绑定接续。开工必需的输入随 Task 保存在 Runner，可在本机读取。 [派发与本地接收](server-runner.md#派发与本地接收)

新 session 按需查询完整讨论、交付文件、操作回执和执行原件。旧 Tend 的 Issue 上下文曾内联历史工具结果，连续调用时会把上次查询结果再次包入正文。后来修复为保留原件引用及按 ID 读取完整正文。 [旧 Tend 故障记录](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/native-event-ingress-and-context-projection.md#L3-L8) · [修复及回归用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/context_tool_originals_test.go#L12-L59)

这条边界沿用首个产品切片的断线 fixture 验证。Work Agent 持久领取后切断 Runner 与 Server 的通信，使用新 session 接续。Fixture 核对新回合收到原目标和约束，实际工作目录与原 Task 一致，原本地操作回执仍可查。完成后沿原 Task 对账并下载交付。 [首个产品切片](plans/first-product-slice.md#最少验收)

## 首期验证

- 用主链 fixture 让 Router 在 Server 本地派发，Issue Agent 委派，Work Agent 回交结果。断开工作 Runner 与 Server 的连接，核对 Router 仍可提交派发，Runner 上的 Agent 仍获得本地受理回执。丢失回执并替换 session 后，用原输入、Issue 和操作 ID 接续。重连并重复递送后，派发、结果和可见状态各只结算一次。
- 用定向工具测试核对身份作用域和回执语义，用真实 App Server 验证 Skill 可见、工具可调用及同一 thread 续跑后的归因。

## 普通报告版本关系

现有 `tend_report_result` 的操作 ID 是普通报告身份。操作者可以用 Server API 或 CLI 补录两个已发布报告之间的版本关系。未来如扩展 `tend_report_result` 允许显式传入前版 ID，应复用 Server 的同 Issue、普通报告、原 Chat 与发布身份、单前驱/后继、无环及原操作幂等规则。只能在两份报告及其精确发布身份均可校验后提交关系，不凭文本、时间或附件猜配，不把版本关系另行投递为消息或等待。本次不新增 Agent 工具参数、不改变 Runner 协议或 Agent 路由。 [普通报告补录](plans/report-version-backfill.md#implemented-contract)

## 独立讨论工具

独立讨论使用 Server 配置的单独本地 App Server。同一讨论 Chat 续用原生 thread，不因引用变化而切换。启动、续用和回合启动都显式选择 `tend-discussion` 权限配置，以 `runtimeWorkspaceRoots` 将当轮目录绑定到只读 `:workspace_roots`。启动和续用时核对 App Server 确认的根目录集合、无审批及无网络的只读沙箱。原生 shell 可读系统最小运行环境、空白工作目录及当轮明确附加的本机仓库或挂载（含验证过的 Git 元数据路径）。服务模型认证在独立 Codex home 或该服务指定的模型密钥文件内。App Server 不继承 Server 或 Router 凭据，原生 shell 不继承该服务环境。讨论不注入工作、发送、Attention、外部订阅或 Issue/Task 修改工具。受控工具是：

- `tend_read_discussion_history` 分页读本讨论当前输入以前的可见消息，不能传 Chat ID。
- `tend_read_discussion_reference` 用同一讨论已附加的身份读精确 Issue 的事实、对话、交付索引，或查询已配置的 GitHub provider。工作目录只由原生文件系统读取。
- `tend_read_discussion_delivery` 核对已附加的确切 Issue 与文件身份，读取同步完成的文本原件。大文件和非文本原件沿原下载入口查看。

GitHub PR 查询沿 Server 的只读凭据。受支持格式的 Codebase 或 GitLab MR 规范化并保留来源链接，目前没有内置 MR 读取 provider，显示不可读原因。已附加 Issue 的作用域不会改变 Router 工具原有的 Chat 检查。 [讨论与转交](chat-routing.md#独立讨论与显式转交)
