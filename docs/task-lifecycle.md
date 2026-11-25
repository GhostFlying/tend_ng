# 任务等待与续跑

**已确认。** Issue 保存持续的目标、Owner 和工作事实。Task 是可由 Runner 独立推进的委托。主动等待暂停当前执行并让出 slot，Task 在待续跑期间保持原身份与目标。

## 工作委派与 Issue 结案

Issue Agent 管理顶层 Issue 的目标、约束和跟进。它通过工作子 Issue 委派实际工作，由 Server 分配 Work Agent，Runner 持久领取后生效。同一份明确交付默认由一个作者 Task 连续调查、设计或实施、验证、吸收反馈和局部修订，阶段交付不拆新 Task。只有工作确可独立并行、或关键结论改变而值得独立重新判断时才拆分或复审。是否拆分、如何审阅由责任 Agent 按目标和仓库约定决定。

**已确认。** 子 Issue 之间的先后顺序、代码起点与交付衔接由 Issue Agent 协调。后续子项需要前一项改动时，Issue Agent 可以安排它在共用 workspace 接着做，也可以为独立 worktree 指定前一项交付的分支或提交。需要整合多项交付并验证时，Issue Agent 可以把这项工作交给合适的工作子 Issue。 [新 worktree 的起点](server-runner.md#新-worktree-的起点)

Work Agent 可以随时提交阶段或最终产物，也可以显式结算自己负责的 Task。Task 结算可与最终产物同一次提交，也允许无产物结算。各 Issue 的结案由各自 Owner 决定。子 Issue 仍在推进时，父 Issue 可以先结案。子 Issue 的实际状态和 Owner 继续保留，停止与资源归还依据相应控制操作和 Runner 的停妥证据。 [Run 正常结束时的 Task 结算](#run-正常结束时的-task-结算)

结案指引要求 Issue Agent 对照原用户目标说明实际交付、尚存风险和有影响的子项后续安排。产物、Task 结算和 Issue 结案沿原操作持久化，界面按 Server 已应用的事实显示。 [Agent 角色](design.md#agent-角色)

用户明确要求当前 Issue 交付计划、报告或附件后原地等待确认或审阅时，Issue Agent 用 `tend_report_result` 上报阶段结果和原件，再用 `tend_request_attention` 登记一个具体、可作答的选择。结束当前 Run，停妥后归还 slot，Issue 保持开放。报告或附件回执不等于结案。答复唤醒原 Task 后，Issue Agent 决定继续还是结案。无明确等待要求时，Owner 仍可在目标达成后上报并结案。 [结果怎样回来](chat-routing.md#结果怎样回来) · [等待期间的用户输入](#等待期间的用户输入)

## 父子 Issue 的等待与唤醒

**已确认。** 顶层 Issue Agent 取得 Runner 的本地持久委派回执后，手头工作处理完即可结束当前回合。Runner 确认停妥后归还 slot，待同步的委派留在本机持久记录里。父 Issue 保留当前 Owner 和工作上下文，等待期间由系统保存待处理输入。

同一个尚未结算的协调目标沿原 Task 接续。等待子项时，原 Task 保留协调责任并让出执行 slot 和新任务准入容量。子项回报成为原 Task 的持久输入，Owner 在下一次开工时使用新的 Run。原协调目标结算后，由后到的独立工作建立新的 Task。

Issue Agent 已持久委派子项后，如果这轮 Run 因 API 等原因失败并停妥，沿原 Run 保留失败证据。仍需跟进的协调目标继续等待子项回报，收到后沿原 Task 启动新 Run，由 Owner 判断后续动作。

派发应用失败沿原派发身份送回发起方。子 Issue 的业务状态改变、Agent 在 Issue Chat 的留言、新交付都必须沿原委派关系送达每一级上级 Issue 的 Owner，直到顶层 Issue Agent。Server 为每个上级持久记录输入，按原事件和目标 Issue 去重。空闲或等待中的 Owner 由输入重新调度，正在执行的 Owner 在下一轮消费。 [旧 Tend 的回传](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/issue-agent-design.md#L81-L84) · [执行 slot](server-runner.md#执行-slot)

文件交付首次在 Server 完整可读时，沿原交付身份生成新交付输入。先到的 Task 结算和留言按各自事实送达。已交付文件的补回沿原身份恢复可用状态。 [交付副本与全量补回](server-runner.md#交付副本与全量补回)

上级 Issue 已结案时仍按必送规则唤醒。顶层 Issue Agent 读到原结案状态和新的子项事实，并显式决定是否重开。子 Issue 的晚到事实和结果始终留在来源子 Issue。纯进展、回执和已有记录或原件的补传只补齐来源事实。新交付以实际发生的交付行为为准。 [Issue Chat 合并展示](chat-routing.md#issue-chat-合并展示)

同一父 Task 的普通输入按固定五分钟窗口调度。第一条普通事实入账时固定截止，窗口内追加不延长。普通包括 Agent 子项留言、新文件首次可读、子 Issue 结案和仍开放的子项成功停妥。所有事实立即持久化并可按原来源查看，到期组内一起投递；父 Task 正忙时由后续 Run 读取。用户留言或转发、Attention 请求和答复、取消与停止控制、失败停妥和派发无可用候选属于即时路径。即时输入可以先唤醒，不会提前放行普通窗口。过期重传与现有原件补回不再计新事实。已终结的父 Task 在普通窗口到期前不因该窗创建后继 Task。

## 执行失败后的接手

**已确认。** Runner 保存原 Run 的失败结果和可核实的原因。确认该 Run 实际停妥后，Runner 在本机结束本次执行并归还 slot，随后同步记录。停妥待确认时，保留关联资源的占用并沿原 Run 对账。 [执行 slot](server-runner.md#执行-slot)

当前 Owner 接手原失败事实和已完成的进展。已明确的 API 或 Provider 故障沿原错误处理，由 Owner 决定如何恢复、重试或结束。执行系统中的非预期异常作为待修复的缺陷，由 Owner 依据原证据跟进。归因未明时保留已有事实供调查。需要上级决定的事项沿原 Issue 逐级上报，顶层需要用户输入时形成决策项。 [逐级上报与用户决策](chat-routing.md#逐级上报与用户决策)

## Run 正常结束时的 Task 结算

旧 Tend 的验收回合正常结束但缺少工具裁决时，Runner 会向原 Agent 补问一次。 [正常回合缺少裁决用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/harness/orca/worker_test.go#L549-L562)

**已确认。** Runner 确认工作 Run 正常结束并实际停妥时，归还 slot。已经在本地受理的显式结算沿原操作同步，已登记的 Task 等待或续作沿原安排接续。其余尚未结算的 Task 由 Runner 根据原 Run 补记一次正常完成，允许零份产物。Runner 持久保存结算和原 Run 的关联，重传沿原结算身份去重。失败和取消按各自的处理规则收尾。 [等待和续跑](#命中事件与续跑) · [执行失败后的接手](#执行失败后的接手)

例如 Work Agent 完成一次只需核对现状的工作，Run 正常结束后即可结算 Task。交付记录按 Agent 实际提交的产物展示，执行说明沿原 Run 按需查看。Task 结算沿原委派关系回传，供上级协调；所属 Issue 由其 Owner 决定是否结案。 [Agent 本地受理](agent-tools.md#本地受理与业务结算) · [执行 slot](server-runner.md#执行-slot)

## 取消与逐级停止

**已确认。** 取消整个 Issue 时，预期沿委派关系逐级停止相关工作。Issue Agent 按原取消范围向下转达，各级 Owner 继续传给自己负责的子项，并为已明确的目标提交停止控制。用户指明子项时，沿该范围处理。

针对 Runner 上执行的停止控制持久投递给目标 Runner。Runner 收到后实际执行停止，停妥证据和控制回执保留原 Task、Run 与操作身份。取消意图、控制送达和实际停妥分别可查。确认停妥后归还 slot，Runner 离线时保留尚待送达的控制。 [Runner 的本地接收](server-runner.md#派发与本地接收)

Server 确认 Task 尚未投递时，可以在本机结束原派发并记录取消。领取结果待确认时，沿原分配核实接收和停止。Runner 已接收的 Task 若仍在本机排队或 `waitfor`，由当前负责该 Task 的 Runner 结束待执行或待续跑安排。原等待和外部事实保留在历史里，相关执行确认停妥后释放目录占用。 [匹配时机与预分配有效期](server-runner.md#匹配时机与预分配有效期)

Issue Agent 按原取消范围发出停止控制后，可以显式将父 Issue 标记为业务已取消。Server 应用后，父 Issue 显示「已取消」，子任务仍按实际证据显示运行、停止待确认或已停。原停止控制继续投递和对账。

## 重开后的接续

**已确认。** Issue Agent 可以显式重开同一个 Issue，保存新输入或重开原因。即使旧子执行仍为「停止待确认」，也可以记录父 Issue 已重开。已有 Task 和执行保留原状态与历史，停止控制及迟到回执继续沿原执行处理。

重开后的工作由 Issue Agent 决定，在原 Host 按 Issue 树当前归属调度。Runner 不可用时等待恢复，缺 slot 时留在队列。复用原 workspace 写入前，由 Runner 根据实际退出或隔离证据核实旧执行已失去写入权。 [Issue 树的 Runner 归属](server-runner.md#issue-树的-runner-归属)

## 独立审阅

**已确认。** Work Agent 可以在当前工作子 Issue 内调用未参与该范围实现的 Codex 原生 subagent 完成自选独审。由 Tend 单独分配审阅时，创建 `review` 子 Issue，由 Review Agent 承担。用户、仓库或平台明确要求的审阅方式仍作用于相应操作。

Runner 通过 App Server 观察实际执行，记录发起线程和子线程的关联。创建回执记录 subagent 已创建。子回合终态或 `wait` 返回的该子 Agent 状态提供实际完成、失败或中断证据。已持久保存的线程历史按原 thread ID 补读，缺少终态证据时保持待确认。 [原生创建与等待事件](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/event_mapping.rs#L97-L134) · [等待结果的状态](https://github.com/openai/codex/blob/cc7ba33601286a751477832b01de7b6f8de0dd9c/codex-rs/app-server-protocol/src/protocol/event_mapping.rs#L224-L258)

Work Agent 显式登记候选提交或产物、审阅范围及结论，引用 Runner 观察到的子线程和本次审阅结果。记录随原工作子 Issue 持久保存，通过现有 Runner 与 Server 同步回传，Issue 详情按需读取。

确需由 Tend 单独安排时，发起方用 `tend_request_review` 绑定候选、范围、原作者 Issue/Task 和审阅 Issue/Task。若作者是工作子项，审阅作为同一协调父 Issue 的子项单独归属。审阅使用独立 workspace，无原作者 workspace 的写入准入。作者结束本轮后，在确认停妥的基础上归还 slot，并保留原 workspace 独占。审阅方用 `tend_deliver_review` 保存结论和证据，Server 沿绑定立即直接回传原作者 Task，同时普通结案和留言仍按父级窗口送达。审阅不能改变任何 Owner 或授权，已经终结的原作者 Task 不自动重开。终结无结论以原审阅 Task 的停止事实回报。要求修改也是有效结论，普通修订在原作者 Task 内完成。用户确认仍走原 Chat 的 Attention。

审阅 Task 和 Run 按通用规则结算。Run 正常结束且实际停妥时，可以在零份交付的情况下结算 Task。执行失败时保留原错误，确认停妥后归还 slot。审阅结论以实际登记的记录为准，发起方按结论或原执行结果决定后续。 [Task 结算](#run-正常结束时的-task-结算) · [执行失败后的接手](#执行失败后的接手)

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
