# Chat 路由与结果回到原对话

Router 的个人助理职责、主 Chat 与 Issue Chat 的对话边界、Attention 和只读通知的用途已确定。提醒时机继续用具体场景核对。

## 用户在 Chat 中看到什么

**已确认。** 用户在主 Chat 始终与同一个助手对话。Router Agent 接收全部用户消息、追问和 Attention 答复，并以同一助手身份在原话题送回答复、交付和需要用户处理的问题。用户可以留在原对话继续交流，需要时按引用查看工作详情。主动在 Issue Chat 发言时，可以直接和对应 Owner 对话。Web 和 CLI 使用同一套对话规则，消息与工作的关联可跨 session 恢复。

发送消息后，Server 持久收件，Chat 显示已收到。Router 在 Server 本地提交派发并取得操作 ID。主 Chat 在同一事务中保存一条回复原输入的受理消息，并附目标 Issue 链接。新派发和向已有 Issue 入队补充都不等 Router 的最终回复或 Runner 领取。仅保存待办、读取回执或派发失败不会生成受理消息。缺少合格 Runner 时，原派发记录显示待分配原因。Owner 开工、处理完成和结果送回 Chat 分别有实际证据。 [Agent 工具与本地受理](agent-tools.md)

需要读仓库或多步核验的项目调查优先派给 `investigation` Agent，已有匹配 Issue 则复用。Router 自行承担耗时工作时，在开始多步工具作业前用 `tend_publish_progress` 给原输入发布一句具体进展。进展和派发回执是完整消息，由正常轮询在当前回合读到。普通直接回答不发进展，原生 commentary 不投递到 Chat。 [验证计划](plans/main-chat-router-feedback.md)

一期的回复和回报以完整消息进入各自的对话。Router 在 Server 写入主 Chat 的回复和回报。工作 Agent 的 Issue 留言在 Runner 落盘后即可进入下一批同步。 [Server 与 Runner 契约](server-runner.md)

## Router 托管与恢复

**已确认。** Server 使用本机 Codex App Server 托管绑定主 Chat 的 Router。主 Chat 输入、各 Issue 上报及归属未定的事实在 Server 本地入队，由 Router 按需消费。工作 Runner 全部离线时，Router 仍可查阅已同步的工作事实、答复用户和提交委派。原委派可以在合格 Runner 恢复时继续分配。

部署方可通过 Server 的 `--router-name` 显式指定主会话助手名称。名称在创建派发时随原消息和目标一起持久保存。未指定或无历史证据时，Issue 时间线只显示「来自主会话」。`router` 是角色 ID，模型、effort、线程、Runner 清单及 Issue Owner 均不能作为历史派发者名称。

Server 从持久记录恢复 Chat 收件、历史读取和 Runner 同步，Router 在本机继续启动或恢复。Router 的 App Server 暂不可用时，保留原输入并显示已收到、待处理。Server 持久保存已提交的回复、派发和操作回执。恢复 Router 时沿原输入和可查的执行证据接续，session 可复用或重建。 [本地受理与业务结算](agent-tools.md#本地受理与业务结算)

## Router 的下一轮输入

旧 Tend 在同一主会话中优先接续已领取的投递，再取用户消息，随后取后台事件。各组按提交顺序处理。回归用例让后台跟进先入队、用户补充后到，验证先领取用户消息。 [旧队列排序](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/store/outbox.go#L56-L68) · [用户消息优先用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/background_test.go#L242-L254)

**首期已确认。** 同一主 Chat 串行启动普通 Router 回合，收件随时持久入队。选择下一轮输入时，已到达的用户消息优先于普通后台回报，用户消息之间按接收顺序处理。已有活跃回合时，新的 Queue 消息等待下一轮。 [Queue 与 Steer](design.md#4-消息投递外部事件与等待)

Router 空闲时按这个顺序选取已到达的输入。一轮可处理多条已到达的 Issue 回报，各自保留原身份和话题。Router 也可以在回复用户时读取相关交付或 Attention，按原约定和紧迫程度决定怎么说。普通工作进展仍可按需查阅。 [结果怎样回来](#结果怎样回来) · [Chat 内处理 Attention](#chat-内处理-attention)

复用一期 A、B 并行、C 继续聊天的 fixture。让 A 的交付在 Router 忙时先入队，随后收到用户的 C，核对 C 持久收件且先进入下一轮，A 的回报仍能沿原话题送达。再让 A 的处理回合先启动，核对 C 持久排队并在 A 回合结束后接续。 [一期验收场景](design.md#6-工程结构与首期验收)

**后续评估。** 吞吐增加后，依据用户消息的排队耗时和实际答复耗时，判断是否用 subagent 分担可异步完成的工作，例如查阅多项 Issue 的交付材料。分工需要让 Router 可以先给用户反馈、结束当前回合并处理下一条消息。子 Agent 的结果回到 Router，由主 Chat 的同一个助手组织答复。

## Router 输入的恢复边界

**已确认。** Chat 收件把原消息、Router 待处理输入和收件回执放在同一个短事务。取得持久的原生 thread 后，Server 在发出 `turn/start` 前记录这次尝试的标识、原输入和 thread，用尝试标识填写 `clientUserMessageId`。原生调用和历史读取在事务外执行。拿到 `turnId` 后沿原尝试保存，回执丢失时可从原 thread 的原生历史按尝试标识查证。 [实际 App Server 验证](plans/rust-offline-smoke.md#router-原生输入的历史关联验证)

| 恢复时持有的证据 | 接续方式 |
| --- | --- |
| Chat 已收件，输入尚未交给 App Server | 从原待处理输入启动 Router。 |
| `turn/start` 没有响应，启动结果待查 | 查原尝试的回合、本机停妥证据和已提交的操作。找到终态就沿原回合结算。确认旧执行已停妥且查不到可恢复的回合时，从同一条输入自动接续一次，保留两次尝试的身份。第二次仍无法恢复时，将该输入标记为处理失败，保留原文和回执，继续同一 Chat 的队列。 |
| Router 的派发已经提交，回合尚无终态 | 自动补发时标明这仍是原请求，携带原消息 ID、尝试编号及已提交的派发。Router 结合实际目标与回执判断哪些工作已经派发，可用 `tend_read_dispatch` 按原操作 ID 读取回执，再继续尚未处理的独立工作。 |
| 原回合已成功结束 | 读齐该回合的输出，按原生消息身份补齐应进入 Chat 的完整消息，再结算该次输入消费。已派发 Issue 的推进和结算沿各自的持久记录继续。 |

一个 Chat 输入可以产生多次合法派发。消息身份锚定用户话题，原生尝试身份关联一次 App Server 执行，业务操作 ID 则用于派发或回复写入的去重。Router 接续时从已提交的派发和回复恢复上下文。 [操作身份及首次回执丢失](agent-tools.md#操作身份及首次回执丢失)

回复某条受理回执时，回执直接链接的 Issue 是明确目标，不混入同一原输入派发的其他 Issue。重建主 Chat 原生线程时，Router 会收到近期直接用户输入对应的已受理工作事实，即使原回合只有回执、没有最终答复。这些事实不表示 Runner 已领取，也不是新授权。临时进展不作为历史回答恢复。

进程仍运行或停妥证据暂不可用时，原尝试保持待查。同一 Chat 的实际执行保持串行，其他 Chat 的 Router 可以继续。原 thread 无法继续时，新 session 从已保存的对话和工作事实接续。用户可以在原 Chat 重新交代处理失败的请求。

旧 Tend 为原生输入保存关联，`turn/start` 结果未知时保留原投递。旧 #387 还暴露了终态查证受实时交付锁阻塞的路径。新版允许历史查证独立读取和补齐原尝试，收件、原进度读取及其他 Issue 的 Runner 同步继续走自己的持久记录。 [旧原生输入和启动结果](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1468-L1505) · [旧历史回执取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L225-L244)

## 从消息确定工作归属

| 用户的输入 | 建议行为 |
| --- | --- |
| 询问已有进展，或提出可以用当前事实回答的问题 | Router 可以直接回答，带上所依据的状态或结果。 |
| 发起需要调查、操作或产出的新工作 | Router 建立或复用顶层 Issue。一人可交付时由 Work Agent 直接领取，独立交付需协调时由 Issue Agent 委派。 |
| 在主 Chat 回复已关联 Issue 的消息 | Router 接收原话和关联，依据内容答复或沿原 Issue 转达。 |
| 主动在某个 Issue Chat 补充要求或追问 | 直接交给对应 Issue 的 Owner。回复子 Issue 留言时沿原留言归属送达。 |
| 同一条消息交代多个独立目标 | Router 可以分别委派。每项工作都有自己的归属，仍能从同一条原消息看到进度和结果。 |
| 确有歧义，选择不同归属会改变目标或授权 | Router 在原 Chat 提一个具体问题，收到答复后接着完成原分发。 |

派发目标保留用户的原目标、具名对象、明确限制和本轮停点，只补必要的执行上下文。未明确写出停点时，用用户要求的结果表示，不另加用户未指定的操作限制。保全依赖所需的仓库特定 rebase、stack 或依赖更新步骤，由 Work Agent 按仓库约定在授权范围内判断和执行。仅当实际效果会实质扩大用户目标或授权时，才带证据请用户确认。只读调查或待确认建议在实施前停下，已有报告不构成实施授权。

**已确认。** 每轮 Router 输入都附上当前 Runner 上报的仓库 ID，已被接替的 Runner 不计入，列表去重排序，未注册仓库时省略。用户明确指定列表中的仓库并要求开始新工作时，Router 用原仓库 ID 和对应顶层 category 调用一次 `tend_dispatch_issue`，不预查候选、不指定 Agent 或 Runner，也不预查 MR / PR。Router 转达具名 MR / PR 和所求结果，Work Agent 使用自己的仓库访问权限查找对象，并按仓库约定执行。审阅请求只授权审阅，交付结论后停下。未明确指定已注册仓库时沿用普通路由。 [实现计划](plans/registered-repo-router-dispatch.md)

同一明确交付的细化、审阅反馈和局部修订优先沿原 Issue、原作者 Task 推进。阶段交付、测试或普通返工不独立派发。Owner 保留原输入，直接 Owner 在原任务处理补充；确实属于已有子项的补充才转交相应 Work Agent。技术步骤由作者自主安排。复用依据可以是明确引用、实际回复关系或能核实的工作上下文。一次路由结果保存原消息、实际派发内容和目标 Issue，重复请求沿原关联恢复。

用户入口和 CLI 支持用 `#N` 或 `N` 查询和操作 Issue。Router 在当前 Chat 内可用编号或 UUID 读取、开始已记录的 Issue，或向其 Owner 转达消息。Server 在入口解析到原 UUID，保存原消息及 Issue 关联。

例如，用户说“调查 A 的崩溃，再整理 B 的发布说明”，Router 可以分别派给两个顶层 Issue。用户在主 Chat 引用 A 的结果补一句“顺便补上这个测试”，Router 带着原话和引用转给 A 的 Issue Agent。用户进入 A 的 Issue Chat 写同一句补充时，可以直接交给 A 的 Owner。

归属未定的待投递 PR / CI 事实进入 Router 的输入队列。Router 可以根据原事实和已有工作确定 Issue 关联，并决定后续行动。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递)

## Issue Chat 合并展示

父 Issue Chat 参考 Multica 的交互，把父 Issue 和下属各级子 Issue 的留言按时间合并到一条时间线。每条留言显示原作者、所属 Issue，并保留原留言身份。合并时间线在同一 Issue 详情页展现并按页读取原留言。没有锚点时加载最新一页，消息锚点沿分页游标定位旧留言。原 Chat 的报告可从详情回到原消息，不改变回复的目标会话。

在父 Issue 发布的新留言发给父 Issue 的 Owner。回复某条子 Issue 留言时，沿被回复留言的归属写入该子 Issue，并送给该 Issue 的 Owner。

用户留言按原收件人及时请求唤醒，等待中的 Owner 同样进入调度。已有活跃回合时，Queue 消息在下一轮处理。 [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

## 结果怎样回来

Work Agent 将交付、证据和剩余事项保存在来源子 Issue。业务状态改变、Agent 留言及新交付沿委派关系从各级上级的持久队列送达，直到顶层 Issue Agent。普通子项事实立即可读，但父 Task 的调度以首条普通事实后的固定五分钟窗口合并。直接用户输入、Attention、控制和明确阻塞及时处理，并在同一目标先释放此前已受理的窗口，不另行开启 Run。绑定的 Tend 审阅结论按本轮请求直接进入原作者 Task。作者显式请求复审时，候选和结果留在原 Review Issue、Task，不另开对话或改责任链。审阅不能代替原 Chat 的用户确认。各顶层 Owner 对照原承诺，将结果和自己的或子项的交付引用分别上报给 Router。直接 Owner 提交的文件可以和报告在同一回合受理，Router 等原件首次可用后才发布该报告。Server 持久入队并保留来源 Issue 和原话题。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

合入请求的交付以仓库既有合入门禁、定向验证和核对远端 main SHA 为终点。仅当用户明确要求时继续等 CI 绿、出包或部署。需要观察时由一位责任人观察并沿责任链共享对应 SHA 与结果，等待已有子项输入时结束当前回合。

Router 按交付约定和当前对话分别转述各 Issue 的结果，也可以重新组织成一条汇总。回报保留原话题及各项结果的引用，后续追问由 Router 沿回复关系接续。候选更新、CI 等待和普通进展保留在工作记录，用户主动问时可以得到当前状态。 同一原话题里，多份报告可以引用同一份原件。各份正文保留，下载入口按原文件身份只显示一次。

用户明确要求当前 Issue 交付计划、报告或附件后原地等待确认或审阅时，Router 在派发的持续目标中注明这是阶段交付，等待用户决定是目标的一部分。Router 先在原 Chat 回报实际结果和附件，再发布通过 `after_report_id` 与报告关联的普通 Attention。无关 Chat 和独立 Attention 不受该依赖阻塞。 若 Router 回合终结仍未发表该阶段报告，Server 在原 Chat 回交 Owner 的原报告和同一组附件，并保留 Router 的失败结果。报告真实发布后，关联问题才可继续。交付回执不是整个 Issue 的完成条件，顶层 Issue 保持开放直到用户作答后由 Owner 决定下一步。 [工作委派与 Issue 结案](task-lifecycle.md#工作委派与-issue-结案)

### 普通报告的版本历史

操作者可显式登记同一 Issue 的两份已发布普通报告为旧→新。原始请求、Chat 与发布通过持久身份确定，不按文字、附件或先后顺序归并。Issue 详情按精确发布关联展示普通报告，无须附件。只有没有精确关联的旧记录才按附件覆盖匹配。普通链只在 Issue 详情呈现当前版本及折叠历史，未关联报告独立展示。每版仍链接自己的原 Chat 消息及已有交付原件。补录不会再次发布，不新增或修改 Chat/Issue 消息、Router 输入、Attention、批准、任务或唤醒。结构化方案仍由独立的 Attention 批准契约处理。 [普通报告补录](plans/report-version-backfill.md#implemented-contract)

## Issue Chat 的留言与执行过程

**已确认。** Issue Chat 里的 Agent 留言由显式调用留言工具产生。用户直接在 Issue 中提问时，Owner 用同一个工具答复并关联原留言。留言按所属 Issue 与原作者持久保存并同步到 Server。派发、交付和 Issue 状态按各自的已提交事实展示。 [Agent 工具与本地受理](agent-tools.md)

派发保留原目标和 Chat 或 Issue 链接。Chat 派发沿原消息区分主会话与其他会话，只有来自主会话且保存了名称才显示名称。Issue 委派展示原操作所属 assignment 中的委派者，不要求委派者是目标的树形父级，例如绑定审阅可由同级作者发起。Runner 的接受确认是另一条只读接手事件，不是回复目标或 Owner 输入，不代表已有 Run。 [派发来源与确认接手](plans/issue-timeline-dispatch-provenance.md)

过程说明、工具调用与执行输出保留在原 rollout。Issue Chat 在对应的执行位置显示「查看执行过程」入口，默认收起。打开消息页只从 Server 读取本页需要的消息和执行引用。用户点开入口时，Web 通过共用 API 向原 Run 所属 Runner 读取这段执行过程。已加载内容可收起，展开时也可以按需刷新。原 Runner 暂不可达时，在对应入口显示读取状态，已同步的留言和交付仍可正常阅读。 [按需读取 rollout](server-runner.md#按需读取-rollout)

查看执行过程是只读操作。上级 Agent 依据持久提交的普通留言、业务状态变化和新交付，在固定窗口到期时收到合并唤醒。普通阶段进展沿原 Issue 保存。旧 Tend 提供默认折叠的执行步骤；一期明确把原件查询放在用户展开时。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒) · [旧执行步骤界面](https://github.com/GhostFlying/tend/blob/83bc15ae/web/src/rollout/RunRollout.tsx#L96-L102)

## 逐级上报与用户决策

各级 Agent 在自己的责任范围内处理能决定的事项。需要上级判断时，沿委派关系向直接上级上报原问题、已知事实和受影响的工作，保留原 Issue 与证据引用。Issue Agent 在权限内可用 `tend_answer_child_attention` 回答原子 Attention。确需用户决定时，用 `tend_escalate_attention` 传原 `attention_id`，保持原问题未答，不代答也不创建替代问题。只有 Issue Agent 自己的新问题才用 `tend_request_attention`。

顶层 Issue Agent 将需要用户决定的事项提交给 Router，Server 持久记录原话题、责任 Agent、问题、已有证据以及已知的选项和影响。Router 用原 Attention ID 在原 Chat 提问，结合任务优先级、时限和延迟答复的影响安排提醒。用户答复由 Router 沿原决策项送回责任 Agent。Work Agent 只有在输入直接证明用户亲答时才归因给用户，否则只陈述决定。依赖答案的动作等待，其余可做的工作照常推进。

派发无候选时，Router 可以根据 Server 保存的原因判断等待、调整选择或请求用户介入。相关问题沿原任务和原 Chat 关联。 [无候选时的处理讨论](agent-selection.md#无候选时的反馈与改选)

## Chat 内处理 Attention

一期优先让用户在 Chat 里完成所有问答和决策。需要用户行动的事项一旦产生便持久记录，用户可以问「还有哪些需要我处理？」。Router 从实际待办读取，每个事项能回到原话题。后续需要更直接的入口时，可以在 Chat 中增加一个可展开的待处理摘要。回复与 CLI 操作使用同一 Attention 和答复状态。

主时间线保留直接对话、约定交付、重要变化和需要提醒的具体问题。紧迫事项由 Router 主动提问。普通待办可随用户追问或适合的汇报合并带回。一次提醒可包含多个事项，每个事项各有处理状态。常规的 CI、运行和排队变化更新原工作的摘要或 Issue 详情。

Issue 详情也可逐项回答已在原 Chat 发布、仍待用户回答且原 Task 可接续的普通 Attention。显式答复用该 Attention ID 和独立操作 ID 提交原文，通过原 `AttentionAnswered` 交给原 Task。原 Chat 在同一问题下显示答复，看板沿原待决事实更新。该操作不新增 Chat 消息、Issue 留言或 Router 输入。普通 Chat 答复和普通 Issue 留言继续走各自原有路径。 [实现与验收](plans/ordinary-attention-issue-reply.md)

## 结构化方案批准

正式上报的报告可以由原 Issue Owner 显式登记为版本化方案。原 Chat 发布对应 Attention，Issue 和原 Chat 共用一个按精确版本提交的批准入口。多个独立待决事项并存时，结构化方案在原 Chat 逐项批准，不在 Issue 流里默选。自由文本不会批准任何结构化方案。结构化批准由 Server 持久记录并直接交给原 Task，不写用户留言或触发 Router。 [实现计划](plans/inline-proposal-approval.md)

## 只读通知

用户打开通知可以浏览已经发生的工作动态，例如 Issue 已开始、进入等待、恢复或结束。通知从已记录的状态变化读取时间、前后状态和工作链接。首期用文本和链接显示，无未读标记，浏览只读取已有记录。需要答复的事项通过 Chat 里的 Attention 处理。

验收时让 A 完成交付、B 产生待处理问题、多个普通 CI 和 Issue 状态变化，同时用户正在 Chat 中写 C。A 的结果只出现一次，B 能从 Chat 找到并回答。CI 变化留在关联工作，Issue 流转可从通知查看，C 的草稿、焦点和阅读位置保留。

旧 Tend 的逻辑会话、按承诺发布结果和同一个 Attention 跨入口接续提供了行为参考。首期用一条 A、B 并行、C 继续聊天的 fixture 核对归属、结果和用户注意力。 [旧主 Agent 职责](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/design.md#L105) · [按约定发布的旧设计](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/quiet-chat-publication.md#L3-L14) · [旧 Attention 接续设计](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/attention-planner.md#L23-L44)

## 独立讨论与显式转交

Web 的「讨论」入口打开 `/discussions`，单个讨论使用 `/discussions?chat=<id>`。普通站内导航留在同一文档，保留 URL、浏览器后退和前进、消息深链及新标签打开。切换页面不会重新提交还在等待回执的消息。Web 从讨论导航或目录创建空白讨论，主 Chat 消息和 Issue 详情标题区不提供快捷创建。需要背景时可在讨论页主动搜索和附加资料，例如 Issue 引用。已有讨论的来源回链及 CLI/API 的来源创建能力保留。标题及来源属于讨论，历史、原生 thread 和输入回执独立绑定该 Chat ID。现有普通 Chat 和 Issue Chat 的历史、收件及派发范围不变。

完整桌面侧栏在「讨论」导航右侧提供独立的「新建讨论」加号。窄窗和移动端从讨论首页新建。仅当前未归档会话在侧栏最近讨论中标记选中。讨论首页可切换到「已归档」列表（`/discussions?archived=true`）。未指定标题时显示「未命名讨论」。首条成功接受的普通用户输入在同一事务内设置自动标题，首尾空白去除、连续 Unicode 空白折叠为一个空格，完整值保留在 SQLite，页面和列表仅在渲染时截断并保留完整文本。引用、来源、原生答复、内部草稿和转交回执不能生成标题。显式创建标题或改名始终按手动标题保存，包括手动改为「未命名讨论」。

`GET /api/v1/discussions` 默认返回未归档讨论。使用 `?archived=true` 仅返回已归档讨论，`?archived=false` 等同默认。列表和详情返回布尔 `archived`。`PATCH /api/v1/discussions/<id>` 接受单独的 `{"archived":true}` 或 `{"archived":false}`，并返回 `chat_id` 和当前设置的 `archived`。改标题继续使用单独的 `{"title":"…"}`。同一设置可重试，不改变活动时间。Web 与 CLI 使用同一接口。

归档当前讨论后，Web 返回普通列表，不创建或自动打开其他会话。可从已归档列表或原链接打开归档会话，恢复后保留同一个页和 Chat ID。归档仅改变列表归属，不删除历史、引用或转交回链，不停止已登记的回合，也不增加读写限制。 [实施及验收计划](plans/discussion-archive.md)

普通讨论发言复用 `/api/v1/chats/<id>/messages`，日常收件、原生回合尝试和恢复沿现有 Chat 模式。讨论不进入普通 Router 工具集，不订阅工作事实，也不派发。已断开的 discussion App Server 有原尝试时先查原生历史。添加、撤销或替换引用不改变同一 Chat 的原生 thread ID，重试也优先续用原 thread。若旧版已清除绑定但仍有原生尝试记录，按最近的尝试查找可续用的 ID，并恢复绑定。每轮在只读 profile 中重新绑定当前所选的本机目录，上一轮撤掉的目录不再可读，但已进入对话的内容不会被删除。只有原生 resume 明确报告已有历史缺失或不可用，才创建新 thread 并从原 Chat 的持久历史接续。只读配置缺失或未被 App Server 确认时不提交回合，也不回退到 Router。 [引用变化与线程接续](plans/discussion-thread-continuity.md)

普通讨论消息页可在对应的用户消息上附带 `discussion_progress`。每项保留原尝试 ID、原生 item ID 和正文。只接收已确认回合中完整的 `agentMessage` commentary。运行中轮询替换最新一行，结算后默认收起，无进展则不占位。进展以纯文本呈现，不生成正式回复，不加入讨论历史工具或转交材料。 [回合进展与恢复](plans/discussion-turn-progress.md)

默认入口是「转交主 Chat」。点击或显式 CLI 调用把转交请求、隐藏输入及当时附加的引用快照一并持久化，不要求填写、预览、确认或选择执行目标。原讨论 Agent 写出一段主 Chat 可直接理解的任务说明、结论或待决请求，只保留必要理由、约束与明确授权，不复述讨论、不套固定字段，也不会把末尾题外话或按钮提示当任务。Server 加简短的非授权提示和原引用快照，并在一个事务内提交转交正文、主 Chat 普通用户消息、Router 输入和双向链接。附加引用不是执行目标，移除引用后不能重新读取已移除的资源。

转交生成优先续用原讨论的原生 thread。登记时同时固定已结算、可见原生回合的前缀。活跃、失败、隐藏和此后新增的回合不得进入旧请求的生成输入。原 thread 含边界外回合时，使用边界前的原生分支，或在无法确认安全前缀、原生历史丢失时，用有界的持久记录恢复。分支不改写正在使用的讨论，不引入新的 Agent 或 summarizer，Discussion Agent 仍使用原只读权限。

新投递由主 Chat 的普通 Router 接收，使用主 Chat 已配置的模型与原生会话。Server 把生成正文与点击前保存的讨论原话分开，给出每条消息的作者角色、ID、原文链接和回复关系。较早原话由 Router 用专用只读工具按同一边界翻页。Router 根据用户原话、修正和限制判断实际目标及停点。原话已明确授权实施就按正常路径分发，不要求再确认；只要求报告、比较或协助决策就停在该范围。只有实际授权或决定不明确时才在主 Chat 提问。生成正文、Agent 建议、引用及点击本身都不授予权限。任何工作写入需引用同讨论内、点击前的真实用户消息 ID。Server 核实出处，Router 判断具体操作是否在原话授权范围内。向现有 Issue 转达时附核实原话及 Router 整理的实际请求与停点。转交不能作为 Attention 答复或结构化批准。

转交回合保留主 Chat 的 Router、模型与持久 thread，同时通过独立的 App Server 进程在只读、无网络的权限下接续原 thread。关闭原生执行、插件与集成，工作写入仅经上述已核实来源的 Tend 工具。不能验证限制时不降级到普通回合。转交引出的报告、Attention 与外部事实保留生成来源并走同样的受限进程，只能回报结果或发布对应待决问题，不能借后台输入开工、改动或代答。普通主 Chat 输入切回原有配置并续用同一 thread。在验收使用的 Codex 0.172 中，原生子代理模式会随 thread 持久化，之后的普通回合可能不再提供原生子代理工具；Tend 的 Issue/Task 派发和普通对话不受影响。原生会话重建时，最近的转交正文和回复作为有来源的背景重放，不充作新的真人指令。

转交回合中断且查不到原生终态时，已提交的工作回执仍可按原 ID 查证。若同一转交已创建或登记 Issue，重放不再新建或登记，即使模型改写了标题或目标。对现有 Issue 开工、退回待办则按原转交消息、动作和目标 Issue 查回原回执，重放不会覆盖用户之后的操作。回执反映原操作，当前状态从 Issue 读取。仍未处理的部分在主 Chat 说明，避免把未完成与重复创建混淆。切换 App Server 时要先确认旧进程停妥。已持久化为旧版只读接收模式的投递不切换执行方式，也不重跑已经完成的历史。旧版在主 Chat 线程之外生成的晚到回复仍沿原有方式补齐上下文。此修订无需 SQLite 迁移；Router 工具修订使旧原生 Router 线程在首次需要时按正常持久回放接续。审阅后手动发送的路径不变。

共享 API 为 `GET/POST /api/v1/discussions/<id>/handoff`。GET 返回当前操作 ID 与请求状态（尚未发起时为 null），也能按 `operation_id` 读历史请求。POST 使用该 `operation_id` 显式登记。同一讨论在下一条真实用户消息前复用相同操作 ID，不因 Agent 回复、引用修改或丢回执重复发送。原操作仍可显式查证和重放。失败不降级投递，提供错误及 `generation`，以同一操作 ID 和失败代次 `retry_generation` 重试。重复重试不启动另一代，原快照和请求边界不变。旧失败与当前提示的操作分别显示，发送新提示后仍可发起新的转交。CLI 在未收到完整回执时保留已经选择的操作 ID，沿原 ID 查证或重试。

「预览 / 编辑」保留独立的手动路径。可以请求模型生成草稿，或不依赖模型手动填写，再编辑待定事项、挑选参考资料与指定主 Chat 范围内的推进 Issue。只有显式发送才投递。原 `/draft` 与 `/handoffs` 以及 CLI `draft`、`draft-read`、`handoff` 继续支持，旧草稿没有待定字段时按空白处理。参考 Issue 可以来自其他 Chat，但不能默认为推进目标。 [实施及验收计划](plans/discussion-quick-handoff-and-titles.md)

## 飞书建议与两种显式动作

飞书建议常驻 Web 与 CLI。Server 在后台采集明确 @ 当前用户的消息并合并同一话题，不自动发主 Chat、通知、Issue 或 Owner。后续回复和来源缺口与研判分开展示。可选具名 `feishu-router` 在 Server 上隔离研判材料，明确已经解决的事项退出待查看。忽略当前版本可撤销，有新来源版本时重新浮现。

- 「带入讨论」只附上所选材料，不发用户消息、不启回合。可以进入已有讨论或创建、继续该建议的讨论。用户后来补充的决定由普通讨论快转按原话授权规则处理。
- 「转交主 Chat」把当前选定材料交给现有只读 Discussion Agent 写说明，再走现有受限 Router 接收。该入口没有原始 Tend 用户指令。外部材料、判断结果和点击本身不授予执行、派发或代答权限。两种动作分别记账，直接转交提供原主 Chat 消息链接，内部快转上下文不增加普通讨论列表项目。

[实施与验收](plans/feishu-suggestions.md)
