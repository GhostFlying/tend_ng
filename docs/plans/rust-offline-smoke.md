# Rust 离线执行 smoke

用隔离的 Rust Server、Runner 和 CLI 验证首个切片。从派发入口建立一条固定 Task。Server 和 Runner 各持 SQLite WAL，Runner 启动固定的 App Server 替身子进程，在临时 workspace 内执行。试验从 Task 派发开始，产品主链按首个产品切片接入。 [首个 Rust 验证切片](../engineering.md#首个-rust-验证切片) · [首个产品切片](first-product-slice.md)

## 验证路径

1. 等 Runner 持久领取、替身到达受控执行点后，断开两端 HTTP。放行替身，核对完整留言和交付文件在 Runner 落盘，停妥证据来自子进程退出，slot 已归还。
2. 恢复通信，通过全量对账回传结果。丢弃第一次已提交的响应，核对重传后仍是原 Task、Run、操作和文件身份，只有一份结算。用 CLI 读取终态并下载原件。
3. 用隔离的 Tailscale 测试控制端验证外置 tsnet 的监听、拨号和状态接续。进程持有各自的临时节点目录，用真实 tsnet 转发到同一 Rust API。重启通信进程后验证相同节点和 API 结果。

试验代码集中放在 `spikes/rust-smoke/`。进程、数据库和网络故障由隔离的本地夹具驱动。App Server 的事件形状以本机生成的协议 schema 为参考，模型输出固定。构建与运行使用 devcontainer。 [通信与升级契约](../server-runner.md#通信通道与独立升级)

## 执行结果

在 Linux 容器内完整通过三轮，分别用时 1.75、1.57、1.57 秒，计时从启动 Server 到清理本轮的临时进程与目录。使用 Rust 1.91.1、Go 1.27.2、Python 3.11.2 和 bundled SQLite。构建使用独立校验的 Rust 工具链和 `golang:1.27-bookworm` 容器。验证运行使用 `--network none`，tsnet 通过容器回环上的测试控制端与 DERP 建立真实连接。

- Server 和 Runner 分别使用独立的 SQLite WAL。在 Runner 持久领取后切断 HTTP。固定 App Server 子进程写出一条完整留言、一份文件和一份显式结算请求，再退出。Runner 落盘终态时，进程已退出且本地运行计数归零，Server 此时尚未收到结果。
- 恢复 HTTP 后，代理丢弃 Server 完成结算的第一份响应。重启 Runner，从原 SQLite 和本地文件读出结果，以字节相同的全量请求重传。Server 只有一条结算和一份交付，CLI 读到原 Task、Run、操作、消息和文件身份，下载字节与原件一致。API 同时验证了错误 token 返回 `401`。
- 两个持不同 `runner_id` 的进程使用同一个外置 tsnet 拨号进程，同时领取各自的 Task。拨号进程停止时，第二个 Runner 在本地完成。重启拨号进程后，节点 ID、tailnet IP 和本机拨号端口一致。第二个 Runner 恢复补传，两个结果均可通过同一路径查询、下载。

App Server 交互覆盖 `initialize`、`thread/start`、`turn/start` 与完整消息、回合完成通知，并通过本机 Codex 0.171.0 生成的 schema 校验。结算由 fixture 单独生成。这个切片从 Task 派发起步，使用固定模型输出、临时 workspace 和测试 tailnet。回传覆盖 Server 缺结果及提交后丢回复的对账，日常同步协议与部署切换需在产品链路中分别验证。

复跑还覆盖原派发存在时补回已经确认的交付，以及结算记录存在时补回单个缺失文件。 [全量对账补回已确认交付](full-reconcile-smoke.md#执行结果)

## 复跑

在有 Rust 1.91.1、Go 1.27.2、Python 3.11 和 C 编译器的 Linux 开发容器内，从项目根目录启动。锁文件记录两侧构建依赖，Go 测试二进制提供独立运行的测试控制端和 tsnet 转发进程。

```sh
cd spikes/rust-smoke
cargo build --locked
go -C transport test -mod=readonly -c -o "$PWD/target/transport.test"
python3 smoke.py target/debug/tend-smoke --transport target/transport.test
```

## Router 原生输入的历史关联验证

使用实际 Codex App Server 0.171.0，在禁用外网的 Linux 容器内接本地 Responses 替身。分别用固定成功输出和固定 HTTP 400 运行一轮。每轮创建持久 thread，用 `clientUserMessageId` 提交输入，回合结束后退出 App Server，再用同一临时数据目录启动新进程。

- 用 `thread/turns/list` 查到原回合，再用 `thread/items/list` 从 `userMessage.clientId` 读回原 `clientUserMessageId`。
- 恢复原 thread，再次发送同一 `clientUserMessageId`。成功和失败两组都产生两个不同的原生 turn ID，模型替身各收到两次请求。
- 第三轮发送 `turn/start` 后让调用端跳过响应读取。模型替身收到请求后挂起返回，再终止该试验的 App Server 进程。从原数据目录重启后，只靠 thread 和 `clientUserMessageId` 查到原回合，历史状态为 `interrupted`，模型替身只收到一次请求。

这次验证支持用 `clientUserMessageId` 关联持久的原生输入。原生调用去重取决于 Tend 在重试前查到的原尝试证据，业务写入结算继续沿 Tend 的操作 ID。 [Router 输入的恢复边界](../chat-routing.md#router-输入的恢复边界)

## Codex 调用 CLI 的执行上下文验证

在禁用外网的 Linux 容器中启动实际 Codex App Server 0.171.0。固定模型输出调用 Codex 的原生 shell，运行同一个替身 `tend`。替身向注入的回环 API 地址发请求，由收件端记录实际地址和测试凭证。

- 同一 App Server、同一工作目录中的两个 thread 使用不同的 `shell_environment_policy.set`，各自的 CLI 请求到达指定地址并带上对应凭证。
- 对仍加载在内存的 thread 调用 `thread/resume` 并传入新配置，下一回合的 CLI 仍使用旧值。测试中 `thread/unsubscribe` 返回成功后，原 thread 在五秒观察窗口内仍出现在 `thread/loaded/list`。
- 从同一数据目录启动新 App Server，恢复原 thread 并传入新配置。下一回合的 CLI 使用新地址和凭证。

这轮验证用于比较 Agent 从 shell 调用 CLI 时的身份和配置接续。 [已确认的 Agent 工具入口](../agent-tools.md#codex-注入-tend-工具)

## Codex 动态工具的执行身份验证

在禁用外网的容器使用 Codex App Server 0.171.0，启用 `experimentalApi`，通过 `thread/start.dynamicTools` 注册探针工具。模型替身返回固定工具调用。探针以收到的 `threadId`、`turnId` 和 `callId` 定位调用，将回执和待投递记录写入本地 SQLite，再返回工具结果。

- 在同一个 App Server 进程复用同一条 thread，连续两轮收到不同的 `turnId`，各自关联到预设的 Run 身份。第二轮跳过 `turn/start` 响应，使用原生 `userMessage` 事件中的 `clientId` 建立尝试与回合的关联，工具调用仍归到正确的 Run。
- 用同一数据目录重启 App Server 后恢复原 thread，原注册工具仍可调用，新调用带有当前回合身份。
- 每轮在探针受理端重放原调用，读回相同操作 ID，SQLite 只保留一次受理和一条待投递记录。运行链路只有本机 App Server、SQLite 和回环模型替身。

这个探针覆盖当前版本的原生来源、工具接续和本地回执形状。实际业务授权、Server 应用与结果补传由产品切片验收。 [Codex 注入 Tend 工具](../agent-tools.md#codex-注入-tend-工具)

## Codex 过程消息和最终答复验证

在隔离容器运行 Codex App Server 0.171.0，回环模型替身在一轮内依次返回过程说明和最终答复。两条文本均收到 `item/completed`，携带的 `phase` 分别为 `commentary` 和 `final_answer`。本机生成的 schema 中 `phase` 可为空。Issue Chat 以显式留言的持久记录展示正文，原生执行记录供点开查看。 [Issue Chat 的留言与执行过程](../chat-routing.md#issue-chat-的留言与执行过程)
