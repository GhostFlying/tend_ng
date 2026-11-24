# 首个产品切片

**第一次试用范围已确认。** 用户在主 Chat 发出一条工作请求，Router 完成分发和收回结果，在原对话留下答复和一份可下载的产物。用一个已登记的示例仓库、一个 Runner、一个执行 slot 和一棵 Issue 树验证。示例仓库作为 workspace 的一个挂载项，采用多仓目录布局。首个可用界面提供纯文本的发送、阅读和下载，CLI 走同一套用户 API 驱动验收。 [总体设计](../design.md#1-产品与交互) · [多仓目录布局](../server-runner.md#多仓目录布局)

## 一期推进顺序

| 里程碑 | 完成标准 |
| --- | --- |
| 持久收件 | 在 devcontainer 用真实 CLI 和 Server，配固定 App Server 输出跑通一问一答。丢掉已落盘的收件响应后用原操作 ID 重试，重启后仍能看到原消息和唯一答复。 [第一段实现](#第一段实现) |
| 第一次试用 | 一个 Runner 和一个 slot 从示例 Git 仓库的 worktree 完成任务。Router 经 Issue Agent 委派给 Work Agent，再在原话题返回结果与可下载文件。CLI 和纯文本 Web 读取同一结果。固定 fixture 覆盖全量对账下的断线交付，并在真实 Codex App Server 上走一次主链。 [输入到交付](#输入到交付) · [最少验收](#最少验收) |
| 可日常使用 | 并行推进两项工作，在原 Chat 收到一项的交付和另一项的 Attention。答复送回责任 Agent，工作可继续。Issue Chat、只读看板和通知反映持久状态，CLI 提供完整操作入口。接入 Queue、Steer、取消与重开，以及 PR / CI 的关联、订阅和等待。真实外部事件的验收先用 GitHub 的 PR 与 CI。 [一期 Attention 验收](#一期-attention-验收) · [一期 Issue Chat 验收](#一期-issue-chat-验收) · [外部事实与等待](../design.md#4-消息投递外部事件与等待) |
| 可独立恢复和升级 | 用普通 API 跑通全量对账和增量同步，核对断线、重启、响应丢失和交付补回后原工作可继续查证。实际部署验证选用通道的鉴权及重新连接。用同机两代 Runner 验证前驱收尾、后继在原目录接续下一次 Run，以及 Server 的协议兼容预检。 [一期同步 API](../server-runner.md#一期同步-api) · [同机 Run 交接](../server-runner.md#同机接替时的-run-交接) · [Server 升级兼容窗口](../server-runner.md#server-升级兼容窗口) |

一期的完成线是在自己的可信单用户环境部署 Server 和工作 Runner，用真实 Codex App Server 从主 Chat 推进 Git 仓库里的日常任务，在原对话取产物、回答 Attention 并继续工作。真实仓库走查核对用户体验和实际副作用，断线、等待和升级各用定向 fixture 核对原身份、目录占用和资源释放。

## 旧 Tend 的参考

旧 Tend 的 Chat 收件已经在事务内写入消息和待投递记录。Server 组装时，用户 API 和 Runner API 共用 `app.App`。实时交付曾持 `workMu` 等原生回合终态，历史收据补查也需要同一把锁。终态只能从历史读回时，两条路径就可能互相等待。新版沿用持久收件，Router 消费、Runner 同步和历史查证各自从持久记录推进。 [旧消息入队](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/app/conversation.go#L185-L193) · [旧 Server 组装](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/serverkit/main.go#L538-L607) · [交付持锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1256-L1267) · [等待回合终态](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/mainthread.go#L1525-L1553) · [历史补查取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L225-L244)

旧 Tend 的单 slot 验收通过业务命令创建 Issue 并请求 Manager，再核对实际领取、委派和交付。沿用它的真实组件和故障注入方式，把新版主链的起点前移到用户发消息，核对 Router 怎么建立原消息和 Issue 的关联。 [旧单 slot 用例](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/e2e/issue_manager_test.go#L154-L156) · [旧用例的起点](https://github.com/GhostFlying/tend/blob/83bc15ae/internal/e2e/issue_manager_test.go#L739-L768) · [旧业务主链测试契约](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/business-flow-contracts.md#L20-L32)

## 工程边界

| 边界 | 负责的事实与操作 |
| --- | --- |
| Server | 持久保存 Chat、Issue 树、派发、业务操作回执和待投递输入。Server 本机托管 Router，按已登记能力和仓库匹配 Runner。 |
| Runner | 持久领取、运行 Issue Agent 和 Work Agent、管理 workspace、记录实际停妥。Runner 上的工具调用先在本机受理，再经同步交给 Server 应用。 |
| 共享 Rust crate | `protocol` 保存线上数据形状和身份，`codex-app` 读写 App Server 原生协议并报告执行事实。Server 和 Runner 各自拥有业务处理、SQLite schema 和事务。 |
| 用户入口与 Agent 工具 | 用户 CLI 和 Web 走共用 Server API。Agent 的工具回调由实际执行实例本机处理。首轮以文本和下载链接呈现用户结果。 |

Server、Runner 分别编译和启动，运行时通过 HTTP 交换持久记录。跨进程用例使用回环网络，外置 tsnet 的监听、拨号与恢复沿用其独立 smoke。 [工程结构](../engineering.md#工程结构) · [Agent 本地受理](../agent-tools.md#本地受理与业务结算) · [外置 tsnet 验证](rust-offline-smoke.md#执行结果)

## 第一段实现

在 devcontainer 从空库启动实际的 Server 和 CLI。根 Rust workspace 先接入主 Chat 用到的 `protocol`、`codex-app`、`tend-server` 和 `tend-cli`。用固定 App Server 替身处理一条可直接回答的 Chat 输入，CLI 经共用用户 API 发送和读取。Server 落盘后立即返回收件回执，Router 通过本机适配器执行，在原话题保存一条完整答复。 [Router 托管与恢复](../chat-routing.md#router-托管与恢复) · [原生输入的历史关联验证](rust-offline-smoke.md#router-原生输入的历史关联验证)

验收时丢弃一次 Server 已落盘的收件响应，CLI 按原操作 ID 重发。核对同一条用户消息只进入一次 Router，重启 Server 后仍能读到原消息和对应答复。接着复用这条用户入口，接上下面的 Issue 派发、一个 Runner slot 的交付和纯文本 Web。

### 持久收件的实现边界

- `protocol` 定义发送、收件回执和分页消息。CLI 通过鉴权 HTTP 调用这些接口，发送前生成操作 ID，可显式沿原 ID 重传。
- Server 用 SQLite WAL 保存完整 Chat 消息、原收件回执、待 Router 处理的输入和原生尝试。收件路径与 Router 消费各有连接，事务在 App Server 通信前结束。一个 Chat 串行消费，其他 Chat 可以独立推进。
- App Server 适配器走本机 stdio，验证使用同一协议的固定替身。按 Chat 复用原生 thread，在 `turn/start` 前保存尝试 ID。正常执行从回合终态和完整消息事件入账，恢复时查原生历史。答复及消费结果同事务保存，保留原消息引用。
- 用实际二进制验证成功答复、收件响应丢失、重启和同 ID 不同输入的冲突。再截断一次已经产生原生输出的启动回执，核对恢复从原生历史补记答复。 [Router 输入的恢复边界](../chat-routing.md#router-输入的恢复边界)
- 为每次本机尝试保存 App Server 进程身份。恢复先查原尝试。确认停妥后，查不到可恢复回合时同一输入至多自动接续一次，原生尝试分别保留。第二次仍无法恢复时，原输入显示失败，下一条仍按顺序处理。替换 thread 时带上有限的近期已完成对话，为后续接入 Issue 摘要与操作回执留出原输入关联。
- 用同一个夹具覆盖成功恢复、重发成功以及连续两次丢失后队列继续。验收时核对原收件回执、原文、尝试次数和之后消息的答复。

在 devcontainer 可以直接复验这一段。

```sh
cargo build --locked
python3 tests/e2e/chat.py target/debug/tend-server target/debug/tend
```

本机运行时用 `TEND_TOKEN` 配置 Server 和 CLI 的同一个预共享 token。启动 `tend-server --db <sqlite-file> --router-cwd <directory>` 后，用 `tend chat send main "Hello"` 收件，再用 `tend chat read main` 查答复。Server 默认调用本机 `codex app-server`，CLI 默认连接本机 HTTP 端口 8832。

## 输入到交付

固定输入为「在示例仓库整理一份使用说明，完成后把文件和简短结论发回来」。测试预置仓库、Runner 和具名 Agent 配置。固定 App Server 输出驱动三个角色按正常接口操作，替身发送带原生回合和调用 ID 的工具回调，由执行端的实际工具入口处理。

1. 用户向主 Chat 发消息。Server 在同一个短事务里写完整消息及 Router 的待处理输入，返回原消息 ID。
2. Router 调用本机工具派发顶层 Issue。Server 保留原消息关联，匹配 `issue-router` 并分配给已登记的 Runner。Runner 持久领取后，Issue Agent 的 Owner 身份生效。
3. Issue Agent 通过工具委派工作子 Issue。Runner 返回本地操作 ID，Server 随后异步应用委派。父 Agent 处理完当前工作并停妥后归还 slot，同一 Runner 持久领取子 Issue 后用这个 slot 启动 Work Agent。
4. Work Agent 从临时 workspace 根启动，沿外层 `AGENTS.md` 进入示例仓库的 worktree 写出文件，经工具提交文件和留言，显式结算 Task 并结案工作子 Issue。Runner 保存原件和受理记录，并根据实际停妥归还 slot。
5. Server 应用子 Issue 回传后，沿原委派关系唤醒 Issue Agent，文件首次可读时按原交付身份入队。Issue Agent 读到交付，显式结案并沿原话题向 Router 上报。Server 保存待处理回报，Router 在原 Chat 发出答复和下载链接。CLI 和纯文本 Web 都能查看同一条完整结果，并从 Server 下载原件。

派发要求和结案动作来自固定 Agent 输出，Server 按正常匹配规则选具名 Agent。测试核对原 Chat 消息、Issue 归属、Task 和 Run、操作身份以及实际产物。 [分配与投递](../server-runner.md#分配与投递) · [工作委派与 Issue 结案](../task-lifecycle.md#工作委派与-issue-结案)

### Router 派发入口

- Router 通过 App Server 注入的工具提交标题、目标和已知的仓库及能力要求。Server 从原生回合与持久尝试确定原 Chat 消息，在短事务中保存顶层 Issue、待匹配 Task 和派发回执。首次领取前可查询实际的待分配状态。
- 工具回调沿原生调用身份返回原回执。启动响应丢失时用原生用户输入事件中的 `clientId` 确认回合归属。原消息需要换 session 接续时，带上已提交的派发引用。
- native thread 的工具声明在创建时绑定。改动工具声明时，新 session 使用当前声明和已保存的相关上下文。已在途的原回合和历史仍按原身份查证。
- 用固定 App Server 输出验证一次正常调用、回执重放以及已提交派发后的回合接续。Chat 消息页带原消息的 Issue 引用，`tend issue read <issue-id>` 读取实际派发及待分配状态。 [Codex 注入 Tend 工具](../agent-tools.md#codex-注入-tend-工具)

## 最少验收

同一输入跑正常路径和断线路径。断线路径在 Work Agent 已持久领取后切断 Runner 与 Server 的通信。Work Agent 仍可写文件、得到本地受理回执并停妥。此时用户可以继续在主 Chat 发消息，Router 根据 Server 已保存的事实回复。重连后先用全量对账补齐，丢弃一次 Server 提交后的响应，再用原操作重传。核对 Work Agent 只执行一次，原 Chat 只有一条 Router 发布的交付，结果可沿来源 Issue 查证，文件可下载，父子 Agent 曾分时使用唯一的 slot。 [首个技术 smoke](rust-offline-smoke.md#执行结果) · [工作 Runner 离线时的主 Chat](../design-questions.md#工作-runner-离线时的主-chat)

在该路径让 Task 结算与停妥证据先到，晚些再完成文件上传。核对先到的事实可以入账，首次可读的交付按原身份入队，重传后 Router 仍在原 Chat 给出一份可下载的交付。 [父子 Issue 的等待与唤醒](../task-lifecycle.md#父子-issue-的等待与唤醒)

模块验收就近覆盖 Server 的收件和幂等应用、Runner 的本地受理和 slot 释放、App Server 的消息解析、Web 的文本交互。影响跨进程接口或交接行为的变更复验对应的进程组合。原生终态迟到时沿原 Run 补查，在执行恢复路径验证。真实 Codex App Server 的 Skill、注入工具和停妥证据沿产品验收核对。 [实现顺序与验证范围](../engineering.md#实现顺序与验证范围)

**无产物结算。** 用 Runner 定向用例模拟只需核对现状的工作。固定输出结束后，核对 Run 正常停妥，原 Task 有一条依据该 Run 记录的完成结算，交付列表为空且 slot 已归还。 [Run 正常结束时的 Task 结算](../task-lifecycle.md#run-正常结束时的-task-结算)

**收件与本地操作的回执。** 分别在 Server 已持久收件、Runner 已持久受理后丢弃第一次响应。用户 API 按原操作重传，核对只产生一条 Chat 消息和一份 Router 输入。工具按原生调用重放，核对只有一条本地操作和一份待同步记录。用定向边界测试覆盖这两个入口故障。 [操作身份及首次回执丢失](../agent-tools.md#操作身份及首次回执丢失)

**Router 恢复。** 在已经发出原生启动请求但 Server 尚未收到响应、以及 Router 派发已提交但回合尚未结束的两个位置截断。在同一份输入上重启 Server，分别核对从历史找到原回合、从操作回执找到原 Issue。恢复之后，原业务操作保留一份派发与关联，主 Chat 可以继续收件。 [Router 输入的恢复边界](../chat-routing.md#router-输入的恢复边界)

**工具执行身份。** 核对 Router 的工具调用进入本机 Server，Issue Agent 和 Work Agent 的工具调用进入本机 Runner。工作 Run 接续旧 thread 时，新调用按实际 `turnId` 归到对应 Run，已经受理的操作沿原 Run 补传。 [Codex 注入 Tend 工具](../agent-tools.md#codex-注入-tend-工具)

## 一期 Attention 验收

**已确认。** 第一次试用后接入 Attention，完成这条链才达到一期验收范围。Work Agent 提出明确问题，沿父子 Issue 上报到顶层 Issue Agent，再交给 Router。Router 在原 Chat 提出可作答的 Attention 并按已定规则提醒。用户从 Web Chat 或 CLI 向 Router 作答，Router 沿原决策项转给负责处理的 Agent。验收核对该 Agent 能从原 Issue 和原答复继续推进，Web 和 CLI 读到同一处理状态。 [逐级上报与用户决策](../chat-routing.md#逐级上报与用户决策)

## 一期 Issue Chat 验收

用户在主 Chat 引用 Router 的回报并补充要求，核对输入先入队给 Router，答复仍在原话题显示为同一个助手。再主动进入同一工作的 Issue Chat 发言，核对原话直接入队给对应 Owner。 [从消息确定工作归属](../chat-routing.md#从消息确定工作归属)

用同一个 fixture 让用户在子 Issue Chat 提问，Owner 输出过程说明并用留言工具答复。打开父 Issue 的合并时间线，核对原作者、回复关系、对应子 Issue 和一条显式答复，同时查看默认收起的执行过程入口。消息页读取只使用 Server 已保存的消息与执行引用。点开入口后，核对唯一的读取目标是对应 Run 的原 Runner，并能看到过程说明与工具执行。核对上级 Agent 的待处理输入来自显式留言，过程查看只读取原记录。原 Runner 不可达时，入口显示读取状态，已同步的留言与交付可继续阅读。 [Issue Chat 的留言与执行过程](../chat-routing.md#issue-chat-的留言与执行过程)
