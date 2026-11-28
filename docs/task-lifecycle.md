# 任务等待与续跑

**已确认。** Issue 保存持续的目标、Owner 和工作事实。Task 是可由 Runner 独立推进的委托。主动等待暂停当前执行并让出 slot，Task 在待续跑期间保持原身份与目标。

时间线里的派发者、历史接手 Agent 和当前 Owner 是不同事实。只在 Server 提交 Runner 的接受回执时展示该 assignment 合同中的接手 Agent。预分配和取件不能提前创建接手事件，后续 Owner 或 Run 也不改写历史。 [派发与投递](server-runner.md#分配与投递)

Issue 详情的「负责 Agent」展示该 Issue 最近确认领取的具名 Owner。首次领取前显示「未领取」。等待、没有活跃 Run 或接续分配尚未确认时保留已生效的归属，子 Issue 不继承父级的 Owner。

## Issue 标题与改名历史

Router 创建或登记 Issue 时，标题写清具体工作和预期结果，不用「调查问题」「处理请求」这类无法辨认的题目。Issue 的标题是当前显示名，不是目标、授权或阶段。Owner 可在初步调查后修正不准确的题目，也可在用户追加使工作实质变化时重估。通常不随阶段或日常进度反复修改。没有次数配额，不通过改标题改变实际工作范围。

已在本地持久领取并实际执行的具名 Owner 可调用 `tend_rename_issue`，只传新标题。Runner 从原 Run 确定 Issue，没有跨 Issue 参数。协调、直接、工作和审阅 Owner 都只能改自己实际领取的 Issue。Router 不持有该工具，不能替 Owner 改现有标题。单用户可通过共享 Bearer 的 `POST /api/v1/issues/<id-or-number>/title` 提交 `operation_id` 和 `title`，不依赖 Issue 是否已领取。Web 仅通过读接口展示，不接受浏览器会话改名。标题去除首尾空白并拒绝空值或内嵌控制字符。规范化后最多 1024 UTF-8 字节，避免单个标题阻塞同步，仍不限制改名次数。

Server 按原 Runner/Assignment/Task/Run、Issue 和最新已接受的 Assignment 应用 Owner 操作。仅预留或发出后继分配不改变 Owner。后继已领取时，前驱晚到的有效原操作只确认收件，不覆盖后继标题。Server 以提交顺序记录旧题、新题、具名 Agent 或用户以及提交时间。原操作重传和同值请求不追加可见事件。改名只更新 `issues.title` 和相应账本，不产生上级输入、Router 唤醒、用户通知或工作阶段变化。

看板、Issue 详情、短号链接和历史引用上的当前 Issue 标签读取当前标题。原派发请求和回执、已发放的 Assignment 合同、历史正文及原生执行记录是原时点快照，不因改名重写。新建 Assignment 读取当时当前标题。详情的独立「改名历史」以同 Issue 的有界分页显示前后值，不占用讨论台消息游标。

## 工作委派与 Issue 结案

顶层 Owner 保留用户原目标、授权和停点。单人交付由 Work Agent 在原顶层 Task 执行，不创建占位子 Issue。协调工作由 Issue Agent 委派，直接 Owner 也可以按需委派真正独立的目标。Server 按 category 分配具名配置，Runner 持久领取后生效。同一份明确交付默认由一个作者 Task 连续调查、设计或实施、验证、吸收反馈和局部修订，阶段交付不拆新 Task。是否拆分由责任 Agent 按工作能否独立推进和仓库约定决定。

只读调查或待确认建议以一份主报告说明根因证据、推荐以及会影响决定的取舍或未知，备查证据按需提供。需要用户作决定时沿现有 Attention 等待，保持原停点。已直接授权的实现按授权完成，不另设调查或审批阶段。

**已确认。** 子 Issue 之间的先后顺序、代码起点与交付衔接由 Issue Agent 协调。后续子项需要前一项改动时，Issue Agent 可以安排它在共用 workspace 接着做，也可以为独立 worktree 指定前一项交付的分支或提交。需要整合多项交付并验证时，Issue Agent 可以把这项工作交给合适的工作子 Issue。 [新 worktree 的起点](server-runner.md#新-worktree-的起点)

Work Agent 可以随时提交阶段或最终产物，也可以显式结算自己负责的 Task。Task 结算可与最终产物同一次提交，也允许无产物结算。各 Issue 的结案由各自 Owner 决定。子 Issue 仍在推进时，父 Issue 可以先结案。子 Issue 的实际状态和 Owner 继续保留，停止与资源归还依据相应控制操作和 Runner 的停妥证据。 [Run 正常结束时的 Task 结算](#run-正常结束时的-task-结算)

结案指引要求顶层 Owner 对照原用户目标说明实际交付、尚存风险和有影响的子项后续安排。产物、Task 结算和 Issue 结案沿原操作持久化，界面按 Server 已应用的事实显示。 [Agent 角色](design.md#agent-角色)

用户明确要求当前 Issue 交付计划、报告或附件后原地等待确认或审阅时，顶层 Owner 用 `tend_report_result` 上报阶段结果和原件，再用 `tend_request_attention` 的 `after_report_id` 引用该次报告操作，登记一个具体、可作答的选择。原 Chat 实际发布该报告后，才可以发布关联问题。独立 Attention 不使用这项依赖。结束当前 Run，停妥后归还 slot，Issue 保持开放。报告或附件回执不等于结案。答复唤醒原 Task 后，Issue Agent 决定继续还是结案。无明确等待要求时，Owner 仍可在目标达成后上报并结案。 [结果怎样回来](chat-routing.md#结果怎样回来) · [等待期间的用户输入](#等待期间的用户输入)

## 父子 Issue 的等待与唤醒

**已确认。** 顶层 Issue Agent 取得 Runner 的本地持久委派回执后，手头工作处理完即可结束当前回合。Runner 确认停妥后归还 slot，待同步的委派留在本机持久记录里。父 Issue 保留当前 Owner 和工作上下文，等待期间由系统保存待处理输入。

同一个尚未结算的协调目标沿原 Task 接续。等待子项时，原 Task 保留协调责任并让出执行 slot 和新任务准入容量。子项回报成为原 Task 的持久输入，Owner 在下一次开工时使用新的 Run。原协调目标结算后，由后到的独立工作建立新的 Task。

Issue Agent 已持久委派子项后，如果这轮 Run 因 API 等原因失败并停妥，沿原 Run 保留失败证据。仍需跟进的协调目标继续等待子项回报，收到后沿原 Task 启动新 Run，由 Owner 判断后续动作。

派发应用失败沿原派发身份送回发起方。子 Issue 的业务状态改变、Agent 在 Issue Chat 的留言、新交付都必须沿原委派关系送达每一级上级 Issue 的 Owner，直到顶层 Issue Agent。Server 为每个上级持久记录输入，按原事件和目标 Issue 去重。空闲或等待中的 Owner 由输入重新调度，正在执行的 Owner 在下一轮消费。 [旧 Tend 的回传](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/issue-agent-design.md#L81-L84) · [执行 slot](server-runner.md#执行-slot)

文件交付首次在 Server 完整可读时，沿原交付身份生成新交付输入。先到的 Task 结算和留言按各自事实送达。已交付文件的补回沿原身份恢复可用状态。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

上级 Issue 已结案时仍按必送规则唤醒。顶层 Issue Agent 读到原结案状态和新的子项事实，并显式决定是否重开。子 Issue 的晚到事实和结果始终留在来源子 Issue。纯进展、回执和已有记录或原件的补传只补齐来源事实。新交付以实际发生的交付行为为准。 [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示)

同一父 Task 的普通输入按固定五分钟窗口调度。第一条普通事实入账时固定截止，窗口内追加不延长。普通包括 Agent 子项留言、新文件首次可读、子 Issue 结案和仍开放的子项成功停妥。所有事实立即持久化并可按原来源查看，到期组内一起投递；父 Task 正忙时由后续 Run 读取。用户留言或转发、Attention 请求和答复、取消与停止控制、失败停妥和派发无可用候选属于即时路径。同一目标接受即时操作时，先把先前已受理的普通窗口和已匹配外部批次释放为有序待送，再与该操作一同提交。该操作之后的新事实另起一批。正常读取不释放窗口，释放本身不创建 Run。顺序取自 Server 的持久收件与可核实的因果关系，不比较 Host 时钟。过期重传与现有原件补回不再计新事实。已终结的父 Task 在普通窗口到期前不因该窗创建后继 Task。

## 执行失败后的接手

**已确认。** Runner 保存原 Run 的失败结果和可核实的原因。确认该 Run 实际停妥后，Runner 在本机结束本次执行并归还 slot，随后同步记录。停妥待确认时，保留关联资源的占用并沿原 Run 对账。 [执行 slot](server-runner.md#执行-slot)

当前 Owner 接手原失败事实和已完成的进展。已明确的 API 或 Provider 故障沿原错误处理，由 Owner 决定如何恢复、重试或结束。执行系统中的非预期异常作为待修复的缺陷，由 Owner 依据原证据跟进。归因未明时保留已有事实供调查。需要上级决定的事项沿原 Issue 逐级上报。Issue Agent 能决定时用 `tend_answer_child_attention` 回答原子 Attention，确需用户决定时用 `tend_escalate_attention` 传原 ID 并保持原问题未答。仅自己的新问题用 `tend_request_attention`。 [逐级上报与用户决策](chat-routing.md#逐级上报与用户决策)

## Run 正常结束时的 Task 结算

旧 Tend 的验收回合正常结束但缺少工具裁决时，Runner 会向原 Agent 补问一次。 [正常回合缺少裁决用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/harness/orca/worker_test.go#L549-L562)

**已确认。** Runner 确认工作 Run 正常结束并实际停妥时，归还 slot。已经在本地受理的显式结算沿原操作同步，已登记的 Task 等待或续作沿原安排接续。其余尚未结算的 Task 由 Runner 根据原 Run 补记一次正常完成，允许零份产物。Runner 持久保存结算和原 Run 的关联，重传沿原结算身份去重。失败和取消按各自的处理规则收尾。 [等待和续跑](#命中事件与续跑) · [执行失败后的接手](#执行失败后的接手)

例如 Work Agent 完成一次只需核对现状的工作，Run 正常结束后即可结算 Task。交付记录按 Agent 实际提交的产物展示，执行说明沿原 Run 按需查看。Task 结算沿原委派关系回传，供上级协调；所属 Issue 由其 Owner 决定是否结案。 [Agent 本地受理](agent-tools.md#本地受理与业务结算) · [执行 slot](server-runner.md#执行-slot)

## 取消与逐级停止

**已确认。** 取消整个 Issue 时，预期沿委派关系逐级停止相关工作。Issue Agent 按原取消范围向下转达，各级 Owner 继续传给自己负责的子项，并为已明确的目标提交停止控制。用户指明子项时，沿该范围处理。

针对 Runner 上执行的停止控制持久投递给目标 Runner。Runner 收到后实际执行停止，停妥证据和控制回执保留原 Task、Run 与操作身份。取消意图、控制送达和实际停妥分别可查。确认停妥后归还 slot，Runner 离线时保留尚待送达的控制。 [Runner 的本地接收](server-runner.md#派发与本地接收)

Server 确认 Task 尚未投递时，可以在本机结束原派发并记录取消。领取结果待确认时，沿原分配核实接收和停止。Runner 已接收的 Task 若仍在本机排队或 `waitfor`，由当前负责该 Task 的 Runner 结束待执行或待续跑安排。原等待和外部事实保留在历史里，相关执行确认停妥后释放目录占用。 [匹配时机与预分配有效期](server-runner.md#匹配时机与预分配有效期)

Issue Agent 按原取消范围发出停止控制后，可以显式将父 Issue 标记为业务已取消。Server 应用后，父 Issue 显示「已取消」，子任务仍按实际证据显示运行、停止待确认或已停。原停止控制继续投递和对账。

## 开放 Issue 退回待安排

已开始且仍开放的 Issue 可原地退回待安排。Server 在同一事务暂停所选子树的调度，沿原 Task 提交停止，并从该操作后的状态决定是否可派发。未投递分配可撤回，Runner 已领取、排队、等待或运行的工作按原证据停妥。API 的受理不证明 Run 已停止。已确认停妥后让出 slot 和目录独占，保留目录、原 Issue 短号、工作、对话、历史 Run、等待记录、交付与 PR 关联。退回不改变 Issue 的业务开闭。

待安排仍可接收事实和留言。迟到的旧执行事实仍在原来源入账，不能凭到期窗口、旧委派、旧决定或旧控制重入调度。Router 调用受原 Chat 作用域约束，CLI 与 Web 共用用户 API。看板仍只读。

等所选子树的旧执行停妥后，显式开工在同一目标 Issue 建立后继 Task，沿原派发要求和当前归属接续。子项仍待安排，由 Owner 决定何时继续。尚未消费的普通事实可交给明确开工的后继。原 Task 的精确等待、Attention 决定和绑定审阅不自动恢复或重投。绑定审阅需由作者明确登记审阅请求。重传原操作 ID 读取原回执，不重放历史操作。

## 重开后的接续

**已确认。** Issue Agent 可以显式重开同一个 Issue，保存新输入或重开原因。即使旧子执行仍为「停止待确认」，也可以记录父 Issue 已重开。已有 Task 和执行保留原状态与历史，停止控制及迟到回执继续沿原执行处理。

重开后的工作由 Issue Agent 决定，在原 Host 按 Issue 树当前归属调度。Runner 不可用时等待恢复，缺 slot 时留在队列。复用原 workspace 写入前，由 Runner 根据实际退出或隔离证据核实旧执行已失去写入权。 [Issue 树的 Runner 归属](server-runner.md#issue-树的-runner-归属)

## 独立审阅

**已确认。** Work Agent 可以在当前原工作 Issue 内调用未参与该范围实现的 Codex 原生 subagent 完成自选独审。由 Tend 单独分配审阅时，创建 `review` 子 Issue，由 Review Agent 承担。用户、仓库或平台明确要求的审阅方式仍作用于相应操作。

调查通常直接交付，用户明确要求复核或新证据可能改变关键结论时，只复核有必要的点。复杂实现建议审阅实际候选。

Runner 通过 App Server 观察实际执行，记录发起线程和子线程的关联。创建回执记录 subagent 已创建。子回合终态或 `wait` 返回的该子 Agent 状态提供实际完成、失败或中断证据。已持久保存的线程历史按原 thread ID 补读，缺少终态证据时保持待确认。 [原生创建与等待事件](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/event_mapping.rs#L97-L134) · [等待结果的状态](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/event_mapping.rs#L224-L258)

Work Agent 显式登记候选提交或产物、审阅范围及结论，引用 Runner 观察到的子线程和本次审阅结果。记录随原工作 Issue 持久保存，通过现有 Runner 与 Server 同步回传，Issue 详情按需读取。

确需由 Tend 单独安排时，发起方用 `tend_request_review` 绑定候选、范围、原作者 Issue/Task 和审阅 Issue/Task。若作者是工作子项，审阅作为同一协调父 Issue 的子项单独归属。审阅在作者所属 Runner 本地排队。作者可在请求被持久受理后继续本轮，Runner 确认实际停妥后才允许审阅取得同一 workspace。审阅结论可先送达原作者 Task，作者仍须等审阅 Run 实际停妥才可开工。原 PR/CI 等待的目录保留不受此交接影响。 [Workspace 分配](server-runner.md#workspace-分配)

`tend_deliver_review` 将结论、证据和来源绑定到本 Run 所审的请求。无结论终结回报对应轮次的原停止或准备失败事实。每轮至多一条直接作者输入，不重开已终结的作者 Task。普通结案和留言仍沿父级投递，用户确认仍走原 Chat Attention。

作者根据结果在原作者 Task 内修订。需要复审时，用 `tend_request_review` 提交不同候选并带结果中的 `child_task_id` 作为 `review_task_id`。已有审阅实际停妥并结算后，新输入让同一 Review Issue、Task 和 Reviewer 再次排队。两轮间归还 slot。审阅 Issue 保持 Owner 原先决定的 open/closed 状态，复审不代为重开或改变 Owner，已取消的审阅不重新执行。原 Task 的普通讨论继续持久保存，结束后的讨论不代替复审请求。候选、范围、请求、证据及结果身份逐轮保留。

Server 在应用请求的事务内核对实际作者 Assignment、未完成的停止控制以及作者和审阅两边的 Issue 祖先。取消或正在停止时，迟到的请求保留本轮未运行回执，不重新排队。重开 Issue 不复活已经停止的作者 Task。

在原 Runner 和兼容配置内优先继续同一原生 thread。明确无法读取原 thread 时，从持久的本轮输入和历史回执新开 session，Issue/Task 不变。连接中断、临时异常或未知恢复结果不证明原 thread 不可用，保留相同准备 Run 重试。同机 Runner 接替后，复审等待前驱原审阅 Assignment 的停妥交接证据，继续使用原具名 Reviewer。只换执行 Assignment，保留原 Issue/Task 和审阅历史，不迁移原生 thread。 [Task 结算](#run-正常结束时的-task-结算) · [Session 接续](agent-tools.md#session-接续)

## PR / CI 的 subscribe 和 waitfor

Agent 的等待入口为 `waitfor`。`subscribe` 和 `waitfor` 共用按来源、对象、适用版本及判定条件描述的匹配。首期支持 PR 状态和 CI 检查，例如等待某个 PR 合入，或等待特定提交的指定检查到达终态。需要区分检查的运行或 attempt 时，条件携带对应身份。

- **`subscribe`** 把命中的事实经所属 Runner 送到订阅者的输入队列。当前执行可以继续，事件按 Optional / Required 和合并唤醒约定处理。
- **`waitfor`** 一次提交具体条件和暂停意图。条件通过校验且尚未满足时，Runner 本地持久保存等待条件、已完成的进展和接下来要做的事，工具返回本地回执。Runner 确认本次执行停妥后归还 slot，已分配的工作目录继续由原 Task 独占。同一目录的其他 Task 等原占用释放。Task 保持等待状态，相关事实随后同步。 [Workspace 分配](server-runner.md#workspace-分配)

Runner 本地已有从 Server 同步的符合本次条件的证据时，`waitfor` 可以返回该证据，当前执行继续。缺少对象或判定条件的调用返回校验错误。

## 命中事件与续跑

Server 持久保存匹配事实，按原等待和 Task 生成直接投递。每一层登记等待的 Agent 都通过当前负责该 Task 接续的 Runner 收到命中事实。同一命中在外部事实批次中按原身份请求一次唤醒。待续跑期间目录占用仍属于原 Task。Runner 确认原占用并取得 slot 后，为原 Task 启动新 Run，在原目录继续。在同一 Runner、兼容配置及原 workspace 内，前一 Run 已停妥时优先恢复同一 Task 的原生 thread。已知不兼容或原 thread 不可用时，从原 Issue、Task、输入、回执和 workspace 新开 session。前次执行未知时不得另启动。若恢复返回旧 thread 仍 active，Runner 持久保留本次预备 Run、未确认消费的输入及原目录独占，重新探测同一 thread，直到可以确认接续。此时丢失 rollout 或更换启动配置不视为停妥证据。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递) · [同机 Run 交接](server-runner.md#同机接替时的-run-交接)

如果匹配事实先到，Server 仍可在收到等待意图后用该事实匹配。旧执行尚未停妥时先保存已命中事实，待可续跑后调度。条件命中依据所等待对象及对应版本的实际证据记录。

旧 Tend 的精确 PR 等待覆盖先捕获事实、实际停妥后续跑同一 Task 的竞态。 [旧 PR 等待竞态](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/pr-external-waits.md#L7-L10)

同一份结构化方案在修订时保留原 Attention 和历史版本。批准使用原 `AttentionAnswered` 唤醒和责任链。Issue 的批准事件取自同一决定记录，有对应输入的原生回合证据后才算接续。 [结构化方案批准](chat-routing.md#结构化方案批准)

Issue 页显式提交的普通 Attention 答复同样固定在原 Task。提交时拒绝已终止或正在停止的原 Task。答复落盘后若原 Task 尚未实际接续便终止，保留答复原文和输入，不为该答复创建后继 Task，也不把答复迁入其他后继工作。只有对应输入的原生回合证据才能显示已经接续。普通 Chat 答复不改变其既有行为。 [Issue 入口](plans/ordinary-attention-issue-reply.md)

## 等待期间的用户输入

**已确认。** `waitfor` 用于释放空闲期间的执行 slot。Router 转来的用户补充、工作方向变更和 Attention 答复，以及用户在 Issue Chat 的直接留言，及时沿原收件关系持久入队并请求唤醒 Owner。其他需要唤醒 Owner 的输入按各自的投递时机进入队列。

已有活跃回合时，Queue 消息在下一轮消费。已停妥让出 slot 时，原 Task 在所属 Runner 上排队，取得 slot 后沿已有目录绑定处理可交付输入。原等待条件及当前证据随输入提供给 Owner，由它决定继续推进、等待或调整工作。用户输入和外部事件同时到达时，保留各自身份，沿原 Task 一起处理已可交付的输入。

## 首期验收

复用主链 fixture，并将 Runner A 容量设为一个 slot。增加一台满足子项要求且空闲的 Runner B。Issue Agent 拿到本地委派回执后，子 Issue 在 A 排队，待同一 Runner 归还 slot 后由 Work Agent 领取。子 Issue 结算后，Issue Agent 用原 Issue 责任恢复。核对每一步的实际运行数、Runner 归属和原回传身份。

在同一主链验证父 Issue 结案时仍有子项未结。子 Issue 晚到的原件补传只补齐原记录。随后模拟业务状态改变、Agent 留言及新交付，核对每一级上级均可处理原事件，顶层状态由 Issue Agent 显式决定。父 Issue Chat 可读到原子 Issue 留言并保留其身份。

复用主链中的用户决策场景，用 API 故障和执行系统异常验证同一收尾路径。在没有结果报告且 Server 断联的场景，Runner 确认失败的 Run 停妥后在本机归还 slot，保存原失败证据。恢复通信后核对故障处置归到原 Owner，并覆盖逐级上报、用户决策项及答复回到原工作。

复用断联 fixture 验证取消和重开。停止控制发出后，父 Issue 可以记录业务已取消，也可在子执行报告停妥前重开。原 Runner 离线期间保留待送控制，新工作在原 Runner 等待。恢复后沿原操作和实际停妥证据结算，核对迟到的控制或回执只作用于原执行，后续执行在原 workspace 可安全复用时取得写入权。

在 `waitfor` fixture 中取消已停妥的 Task，核对该 Task 结算与目录释放。随后送达原条件命中的 CI 事实，沿原身份记录外部结果，确认原 Task 的执行历史仍只有原 Run。

用一台只有一个 slot 的 Runner。Task A 对具体 CI 条件调用 `waitfor`，停妥后归还 slot。使用独立 worktree 的 Task B 取得 slot，使用 A 的工作目录的 Task C 等待目录。A 的匹配事件到达后，A 保持目录占用并排队等 slot。B 停妥后，以新 Run 在原目录续跑 Task A。复用同一 fixture 验证事件先到、等待或停妥后到，以及重复投递时的单次续跑。用局部测试核对缺少条件的校验错误和已有匹配证据时的直接返回。

复用该 fixture，在 CI 尚未命中时向 A 发送普通用户补充。A 在 slot 可用时沿原 Task 处理消息，原等待条件仍可查。补充和匹配事件同时成为可交付输入时，核对一次实际续跑读到两份原输入。

用 App Server fixture 验证原生审阅的创建、等待返回时子 Agent 仍在执行、子回合实际结束三个事实。用原父子线程身份重放，核对审阅记录与候选和结果仍归原工作子 Issue。
