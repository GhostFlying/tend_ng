# Chat 路由与结果回到原对话

Router 的个人助理职责、主 Chat 与 Issue Chat 的对话边界、Attention 和只读通知的用途已确定。提醒时机继续用具体场景核对。

## 用户在 Chat 中看到什么

**已确认。** 用户在主 Chat 始终与同一个助手对话。Router Agent 接收全部用户消息、追问和 Attention 答复，并以同一助手身份在原话题送回答复、交付和需要用户处理的问题。用户可以留在原对话继续交流，需要时按引用查看工作详情。主动在 Issue Chat 发言时，可以直接和对应 Owner 对话。Web 和 CLI 使用同一套对话规则，消息与工作的关联可跨 session 恢复。

发送消息后，Server 持久收件，Chat 显示已收到。Router 在 Server 本地提交派发并取得操作 ID。Issue 关联与 Owner 的待投递输入已持久入队后，Chat 显示已派发。缺少合格 Runner 时，原派发记录显示待分配原因。Owner 开工、处理完成和结果送回 Chat 分别有实际证据。平常的排队和运行进展显示在关联工作上，用户需要时可以展开。 [Agent 工具与本地受理](agent-tools.md)

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

进程仍运行或停妥证据暂不可用时，原尝试保持待查。同一 Chat 的实际执行保持串行，其他 Chat 的 Router 可以继续。原 thread 无法继续时，新 session 从已保存的对话和工作事实接续。用户可以在原 Chat 重新交代处理失败的请求。

旧 Tend 为原生输入保存关联，`turn/start` 结果未知时保留原投递。旧 #387 还暴露了终态查证受实时交付锁阻塞的路径。新版允许历史查证独立读取和补齐原尝试，收件、原进度读取及其他 Issue 的 Runner 同步继续走自己的持久记录。 [旧原生输入和启动结果](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1468-L1505) · [旧历史回执取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L225-L244)

## 从消息确定工作归属

| 用户的输入 | 建议行为 |
| --- | --- |
| 询问已有进展，或提出可以用当前事实回答的问题 | Router 可以直接回答，带上所依据的状态或结果。 |
| 发起需要调查、操作或产出的新工作 | Router 建立或复用顶层 Issue，Issue Agent 收到原目标和约束，并通过工作子 Issue 委派给 Work Agent。 |
| 在主 Chat 回复已关联 Issue 的消息 | Router 接收原话和关联，依据内容答复或沿原 Issue 转达。 |
| 主动在某个 Issue Chat 补充要求或追问 | 直接交给对应 Issue 的 Owner。回复子 Issue 留言时沿原留言归属送达。 |
| 同一条消息交代多个独立目标 | Router 可以分别委派。每项工作都有自己的归属，仍能从同一条原消息看到进度和结果。 |
| 确有歧义，选择不同归属会改变目标或授权 | Router 在原 Chat 提一个具体问题，收到答复后接着完成原分发。 |

派发保留用户的原目标、授权边界和本轮停点，只补必要的执行上下文。只读调查或待确认建议在实施前停下，已有报告不构成实施授权。

**已确认。** 每轮 Router 输入都附上当前 Runner 上报的仓库 ID，已被接替的 Runner 不计入，列表去重排序，未注册仓库时省略。用户明确指定列表中的仓库并要求开始新工作时，Router 用原仓库 ID 调用一次 `tend_dispatch_issue`，不预查候选、不指定 Agent 或 Runner，也不预查 MR / PR。Work Agent 使用自己的仓库访问权限查找对应对象。审阅请求只授权审阅，交付结论后停下。未明确指定已注册仓库时沿用普通路由。 [实现计划](plans/registered-repo-router-dispatch.md)

同一明确交付的细化、审阅反馈和局部修订优先沿原 Issue、原作者 Task 推进。阶段交付、测试或普通返工不独立派发。Issue Agent 保留原输入，把影响实现的补充送给已有工作子 Issue，技术步骤由 Work Agent 自主安排。复用依据可以是明确引用、实际回复关系或能核实的工作上下文。一次路由结果保存原消息、实际派发内容和目标 Issue，重复请求沿原关联恢复。

用户入口和 CLI 支持用 `#N` 或 `N` 查询和操作 Issue。Router 在当前 Chat 内可用编号或 UUID 读取、开始已记录的 Issue，或向其 Owner 转达消息。Server 在入口解析到原 UUID，保存原消息及 Issue 关联。

例如，用户说“调查 A 的崩溃，再整理 B 的发布说明”，Router 可以分别派给两个顶层 Issue。用户在主 Chat 引用 A 的结果补一句“顺便补上这个测试”，Router 带着原话和引用转给 A 的 Issue Agent。用户进入 A 的 Issue Chat 写同一句补充时，可以直接交给 A 的 Owner。

归属未定的待投递 PR / CI 事实进入 Router 的输入队列。Router 可以根据原事实和已有工作确定 Issue 关联，并决定后续行动。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递)

## Issue Chat 合并展示

父 Issue Chat 参考 Multica 的交互，把父 Issue 和下属各级子 Issue 的留言按时间合并到一条时间线。每条留言显示原作者、所属 Issue，并保留原留言身份。合并时间线在同一 Issue 详情页展现并按页读取原留言。没有锚点时加载最新一页，消息锚点沿分页游标定位旧留言。原 Chat 的报告可从详情回到原消息，不改变回复的目标会话。

在父 Issue 发布的新留言发给父 Issue 的 Owner。回复某条子 Issue 留言时，沿被回复留言的归属写入该子 Issue，并送给该 Issue 的 Owner。

用户留言按原收件人及时请求唤醒，等待中的 Owner 同样进入调度。已有活跃回合时，Queue 消息在下一轮处理。 [等待期间的用户输入](task-lifecycle.md#等待期间的用户输入)

## 结果怎样回来

Work Agent 将交付、证据和剩余事项保存在来源子 Issue。业务状态改变、Agent 留言及新交付沿委派关系从各级上级的持久队列送达，直到顶层 Issue Agent。普通子项事实立即可读，但父 Task 的调度以首条普通事实后的固定五分钟窗口合并。直接用户输入、Attention、控制和明确阻塞及时处理。绑定的 Tend 审阅结论按本轮请求直接进入原作者 Task。作者显式请求复审时，候选和结果留在原 Review Issue、Task，不另开对话或改责任链。审阅不能代替原 Chat 的用户确认。各顶层 Issue Agent 对照原承诺，将结果和交付引用分别上报给 Router。Server 持久入队并保留来源 Issue 和原话题。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒)

Router 按交付约定和当前对话分别转述各 Issue 的结果，也可以重新组织成一条汇总。回报保留原话题及各项结果的引用，后续追问由 Router 沿回复关系接续。候选更新、CI 等待和普通进展保留在工作记录，用户主动问时可以得到当前状态。 同一原话题里，多份报告可以引用同一份原件。各份正文保留，下载入口按原文件身份只显示一次。

用户明确要求当前 Issue 交付计划、报告或附件后原地等待确认或审阅时，Router 在派发的持续目标中注明这是阶段交付，等待用户决定是目标的一部分。Router 在原 Chat 回报实际结果和附件，并发布相关 Attention。交付回执不是整个 Issue 的完成条件，顶层 Issue 保持开放直到用户作答后由 Owner 决定下一步。 [工作委派与 Issue 结案](task-lifecycle.md#工作委派与-issue-结案)

### 普通报告的版本历史

操作者可显式登记同一 Issue 的两份已发布普通报告为旧→新。原始请求、Chat 与发布通过持久身份确定，不按文字、附件或先后顺序归并。Issue 详情按精确发布关联展示普通报告，无须附件。只有没有精确关联的旧记录才按附件覆盖匹配。普通链只在 Issue 详情呈现当前版本及折叠历史，未关联报告独立展示。每版仍链接自己的原 Chat 消息及已有交付原件。补录不会再次发布，不新增或修改 Chat/Issue 消息、Router 输入、Attention、批准、任务或唤醒。结构化方案仍由独立的 Attention 批准契约处理。 [普通报告补录](plans/report-version-backfill.md#implemented-contract)

## Issue Chat 的留言与执行过程

**已确认。** Issue Chat 里的 Agent 留言由显式调用留言工具产生。用户直接在 Issue 中提问时，Owner 用同一个工具答复并关联原留言。留言按所属 Issue 与原作者持久保存并同步到 Server。派发、交付和 Issue 状态按各自的已提交事实展示。 [Agent 工具与本地受理](agent-tools.md)

派发保留原目标和 Chat 或 Issue 链接。Chat 派发沿原消息区分主会话与其他会话，只有来自主会话且保存了名称才显示名称。Issue 委派展示原操作所属 assignment 中的委派者，不要求委派者是目标的树形父级，例如绑定审阅可由同级作者发起。Runner 的接受确认是另一条只读接手事件，不是回复目标或 Owner 输入，不代表已有 Run。 [派发来源与确认接手](plans/issue-timeline-dispatch-provenance.md)

过程说明、工具调用与执行输出保留在原 rollout。Issue Chat 在对应的执行位置显示「查看执行过程」入口，默认收起。打开消息页只从 Server 读取本页需要的消息和执行引用。用户点开入口时，Web 通过共用 API 向原 Run 所属 Runner 读取这段执行过程。已加载内容可收起，展开时也可以按需刷新。原 Runner 暂不可达时，在对应入口显示读取状态，已同步的留言和交付仍可正常阅读。 [按需读取 rollout](server-runner.md#按需读取-rollout)

查看执行过程是只读操作。上级 Agent 依据持久提交的普通留言、业务状态变化和新交付，在固定窗口到期时收到合并唤醒。普通阶段进展沿原 Issue 保存。旧 Tend 提供默认折叠的执行步骤；一期明确把原件查询放在用户展开时。 [父子 Issue 的等待与唤醒](task-lifecycle.md#父子-issue-的等待与唤醒) · [旧执行步骤界面](https://github.com/GhostFlying/tend/blob/83bc15ae/web/src/rollout/RunRollout.tsx#L96-L102)

## 逐级上报与用户决策

各级 Agent 在自己的责任范围内处理能决定的事项。需要上级判断时，沿委派关系向直接上级上报原问题、已知事实和受影响的工作，保留原 Issue 与证据引用。上级依据自己的责任判断，必要时继续向上报。

顶层 Issue Agent 将需要用户决定的事项提交给 Router，Server 持久记录原话题、责任 Agent、问题、已有证据以及已知的选项和影响。Router 在原 Chat 提出可作答的 Attention，结合任务优先级、时限和延迟答复的影响安排提醒。用户答复由 Router 沿原决策项送回责任 Agent。依赖答案的动作等待，其余可做的工作照常推进。

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

Web 的「讨论」入口打开 `/discussions`，单个讨论使用 `/discussions?chat=<id>`。普通站内导航留在同一文档，保留 URL、浏览器后退和前进、消息深链及新标签打开。切换页面不会重新提交还在等待回执的消息。Web 可从空白创建，或从主 Chat 的原消息创建。Issue 详情标题区不提供快捷创建，可在讨论页主动搜索并附加 Issue 引用。标题及来源属于讨论，历史、原生 thread 和输入回执独立绑定该 Chat ID。现有普通 Chat 和 Issue Chat 的历史、收件及派发范围不变。

完整桌面侧栏在「讨论」导航右侧提供独立的「新建讨论」加号。窄窗和移动端从讨论首页新建。仅当前未归档会话在侧栏最近讨论中标记选中。讨论首页可切换到「已归档」列表（`/discussions?archived=true`）。未指定标题时显示「未命名讨论」，显式标题不按文字值改写。

`GET /api/v1/discussions` 默认返回未归档讨论。使用 `?archived=true` 仅返回已归档讨论，`?archived=false` 等同默认。列表和详情返回布尔 `archived`。`PATCH /api/v1/discussions/<id>` 接受单独的 `{"archived":true}` 或 `{"archived":false}`，并返回 `chat_id` 和当前设置的 `archived`。改标题继续使用单独的 `{"title":"…"}`。同一设置可重试，不改变活动时间。Web 与 CLI 使用同一接口。

归档当前讨论后，Web 返回普通列表，不创建或自动打开其他会话。可从已归档列表或原链接打开归档会话，恢复后保留同一个页和 Chat ID。归档仅改变列表归属，不删除历史、引用或转交回链，不停止已登记的回合，也不增加读写限制。 [实施及验收计划](plans/discussion-archive.md)

普通讨论发言复用 `/api/v1/chats/<id>/messages`，日常收件、原生回合尝试和恢复沿现有 Chat 模式。讨论不进入普通 Router 工具集，不订阅工作事实，也不派发。已断开的 discussion App Server 有原尝试时先查原生历史。添加、撤销或替换引用不改变同一 Chat 的原生 thread ID，重试也优先续用原 thread。若旧版已清除绑定但仍有原生尝试记录，按最近的尝试查找可续用的 ID，并恢复绑定。每轮在只读 profile 中重新绑定当前所选的本机目录，上一轮撤掉的目录不再可读，但已进入对话的内容不会被删除。只有原生 resume 明确报告已有历史缺失或不可用，才创建新 thread 并从原 Chat 的持久历史接续。只读配置缺失或未被 App Server 确认时不提交回合，也不回退到 Router。 [引用变化与线程接续](plans/discussion-thread-continuity.md)

`/api/v1/discussions` 提供列表、幂等创建、标题、引用、候选查找、Task 工作目录选择和转交。用户以「交给 Router」显式请求模型形成可编辑的草稿，草稿回合独立保存在原讨论，不插入用户可见消息或主 Chat。用户检查目标、已明确决策、约束和授权终点，另选要发送的参考资料与确实要推进的 Issue。执行目标只接受主 Chat 范围内的 Issue，参考 Issue 可以从其他 Chat 附加。发送使用稳定操作 ID，每份草稿最多送达一次；同一请求重试返回原收件，不同请求复用原 ID 时返回冲突。主 Chat 收到正常用户消息，现有 Router 保留是否开工、推进或澄清的判断。主 Chat 消息和原讨论持久双向链接，普通答复和派发回执沿主 Chat 原逻辑返回。 [实施及验收计划](plans/discussion-router-handoff.md)
