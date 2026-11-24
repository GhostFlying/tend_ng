# Tend NG 基础选型与工程结构

首期以一个活动 Server 和多个独立 Runner 跑通 Chat、分发、执行和回收。本文记录基础选型、构建边界和实现顺序，首轮实现按产品切片计划推进。 [总体设计](design.md) · [首个产品切片](plans/first-product-slice.md)

## 基础技术栈

| 部位 | 首期基线 | 选型依据 |
| --- | --- | --- |
| **Server、Runner、CLI** | Rust，三个独立构建入口 | CLI 和各服务分别构建、运行和替换。 |
| **通信** | 独立的 Go tsnet 进程、LAN 直连、宿主机 `tailscaled` | 各通道承载同一套 HTTP 协议。tsnet 节点状态和连接转发由通信进程负责。 |
| **用户 API 和同步** | HTTP/JSON。Web 定期查询，Runner 定期通信 | CLI 与 Web 调同一套用户 API。Server 与 Runner 通过全量对账和批量增量 API 同步完整记录。 |
| **Web** | React、TypeScript、Vite，静态构建同源部署 | Chat 时间线、草稿、只读通知和 Issue 讨论有各自直接可测的组件边界。旧 Tend 的界面组织可以作为参考。 |
| **Codex** | Server 和 Runner 各自使用本机 Codex App Server | Server 托管 Router，Runner 托管 Issue Agent、Work Agent 和 Review Agent。CLI 和 Web 从 Tend API 读取执行事实。 |
| **开发环境** | devcontainer 中固定后端工具链、Node、包管理器和 Codex 版本 | 同一环境能启动独立的 Server 与 Runner，并用回环地址注入断线、重连和延迟。实际通道在部署环境做连通和鉴权验证。 |

devcontainer 统一提供 Rust 和 Go 工具链。Web 工具链跟随选中的 Vite 版本。 [tsnet Go API](https://pkg.go.dev/tailscale.com/tsnet) · [Vite 工具链要求](https://vite.dev/guide/#scaffolding-your-first-vite-project) · [Vite 静态构建](https://vite.dev/guide/build)

## Go 与 Rust

**已确认。** Server、Runner 和 CLI 采用 Rust。tsnet 采用 Go 并作为独立进程构建、运行和更新，Rust 业务进程通过本机网络连接使用它。 [独立 tsnet 进程](#独立-tsnet-进程) · [Smoke 结果](plans/rust-offline-smoke.md#执行结果)

**旧 Tend 的并发和测试证据。** 负责人实时交付与历史回执查证会争用同一把锁。配置 watcher 也曾使用不同的锁写同一个持久状态，修复把手动控制和失败诊断放回同一写入边界。此类时序问题由短事务、独立执行与原身份对账处理。Safe Rust 的所有权和类型系统可以在编译时约束进程内的共享可变内存，锁顺序和跨进程结算仍由设计与测试验证。 [历史回执取锁](https://github.com/GhostFlying/tend/blob/2096f5c213e7c64dfd52c3d0ba6709dd287e8fae/internal/mainthread/turn_receipt.go#L222-L244) · [并发状态写入修复](https://github.com/GhostFlying/tend/commit/578c6672155c9a895286d5f1141f4bfac10e7d63) · [Rust 的数据竞争](https://doc.rust-lang.org/nomicon/races.html)

历史维护测量中，同一条 App 测试普通运行约 0.10–0.14 秒，启用 `-race` 后约 2.88–2.96 秒。带插桩的三次样本平均 2.92 秒，其中 SQLite 迁移约 2.68 秒。CI #423 的 App 包超过 53 分钟。#175 改为从真实迁移生成的空库模板复制到各测试的私有目录，保留独立数据库及专门的迁移用例。新版按模块运行定向测试，并保持少量需要真实进程和存储的主链 fixture。 [CI #423](https://github.com/GhostFlying/tend/actions/runs/41017673704) · [空库模板修复 #175](https://github.com/GhostFlying/tend/pull/175)

**日常验证。** Rust 业务模块运行定向 `cargo test`。领取冲突、重复投递、丢回执和断线续跑使用可控制时序的 fixture。Go 通信进程有自己的监听、拨号和状态恢复测试。比较日常迭代耗时时，分别记录编译、SQLite fixture 初始化和测试执行时间。 [Rust 的数据竞争](https://doc.rust-lang.org/nomicon/races.html) · [Go race detector 开销](https://go.dev/doc/articles/race_detector#Runtime_Overhead)

### 首个 Rust 验证切片

**已验证。** 技术验证从一条已派发的 Task 开始，使用隔离的 Rust Server、Runner、本地 SQLite 库和临时 workspace。固定 fixture 模拟 App Server 的输出，回环 HTTP 提供可控制的断线。沿用旧 Tend 离线验收中从实际领取到原结果回传的路径。 [旧离线执行验收](https://github.com/GhostFlying/tend/blob/83bc15ae/docs/plans/offline-runner-execution.md#L17-L19)

1. 从 Server 的派发入口提交固定 Task，等 Runner 持久领取后断开两端通信。
2. Runner 在本机完成 fixture，保存一条完整留言、一个交付文件和停妥证据，归还执行 slot。
3. 恢复通信，只用全量对账补齐原记录。重复递送，核对 CLI 查到同一个 Task、一次结算和同一份可下载交付。

本地 smoke 已在隔离开发容器内通过三轮。覆盖离线推进、提交后丢回执、Runner 重启后的全量重传，以及同一套 HTTP 接口通过真实 tsnet 监听、拨号和节点恢复后补传。产品主链再把 Chat、Router 和 Issue Agent 接到同一派发与回传入口。 [验证结果和复跑命令](plans/rust-offline-smoke.md#执行结果)

### 独立 tsnet 进程

**已确认。** 独立 Go 进程持有 tsnet 节点状态并负责入网、监听、拨号和连接转发。同一 Host 的两代 Runner 可共用这一条通道，各自持有 `runner_id`、本地库和执行记录。通信进程独立发布和重启。宿主机已有 `tailscaled` 时，系统联网和端口转发也能提供这种接入方式。 [tsnet 持久状态](https://tailscale.com/docs/features/tsnet/how-to/create-basic-tsnet-app#control-the-directory-for-persistent-data) · [tsnet 拨号](https://pkg.go.dev/tailscale.com/tsnet#Server.Dial) · [系统 TCP 转发](https://tailscale.com/docs/reference/tailscale-cli/serve#use-a-tcp-forwarder)

- 两代 Runner 的出站同步可以共用同一个本机拨号入口，Server 沿请求中的原 Runner 身份与回执结算。
- 入站按各 Runner 的监听端点转发。例如在同一个 tailnet 节点开放两个端口，分别指向前驱和后继的本机 API。Server 按 `runner_id` 查到对应端点，从原 Runner 读取 rollout。 [tsnet 监听](https://pkg.go.dev/tailscale.com/tsnet#Server.Listen) · [按需读取 rollout](server-runner.md#按需读取-rollout)

这个进程可在 Runner 接替期间持续运行，前驱和后继可同时鉴权和同步。两代 Runner 共用这个进程的可用性，进程中断时各自按本机执行契约继续已领取工作，恢复后沿原身份同步。 [通信通道与独立升级](server-runner.md#通信通道与独立升级)

验证这条 Rust 接入路径时，用同一个节点让两个 Runner 同时同步，从 Server 分别读取两者的原 Run。仅替换其中一个 Runner，核对另一实例的连接、身份和原件读取。通信进程单独重启时，核对它从原节点状态恢复，两代 Runner 沿原进度继续同步。

## Web 工具链

以旧 Tend 主干 `83bc15ae` 的 Web 工具链为初始基线。开发容器使用 Node 24，包管理用 pnpm 10.34.7。旧版锁文件中的 React、TypeScript 和 Vite 分别为 19.3.1、6.0.3 和 8.3.5。ESLint 和 Prettier 负责静态检查和格式，Vitest 跑模块测试，Playwright 验证少量关键浏览器路径。新项目按实际前端依赖维护自己的清单和锁文件。 [旧容器工具链](https://github.com/GhostFlying/tend/blob/83bc15ae/.devcontainer/Dockerfile#L1-L20) · [旧前端清单](https://github.com/GhostFlying/tend/blob/83bc15ae/web/package.json) · [旧前端锁文件](https://github.com/GhostFlying/tend/blob/83bc15ae/web/pnpm-lock.yaml#L9-L71)

## 数据库

**已确认。** 一期 Server 和每个 Runner 各自使用本地 SQLite WAL。Server 保存权威的完整 Chat 正文和 Issue 当前状态。Runner 持有自己的输入、执行与发送记录，在本机确认已接收的指令、原生回执和补传位置。SQL 与事务由各组件的存储实现持有。Smoke 已验证 `rusqlite` 的 bundled SQLite 配置，作为产品实现的驱动候选。 [数据归属与访问边界](storage.md) · [Smoke 结果](plans/rust-offline-smoke.md#执行结果)

用可重放的组合负载验证有界查询、短事务和用户消息入队延迟。在预期负载下，即使事务足够短，互不相关的 Issue 写入仍持续拖慢 Chat 或 Attention 时，再沿 Server 存储边界评估切换 PostgreSQL。 [数据库选型与验证](storage.md#选型和验收)

## 工程结构

以下目录草图说明独立构建入口和共享边界。

```text
Cargo.toml          Rust workspace
crates/
  tend-server/      Server 入口、用户 API、Router 托管、工作归属、投递、外部事实采集、Server 存储
  tend-runner/      Runner 入口、输入接收、执行、workspace、本地恢复、Runner 存储
  tend-cli/         CLI 入口
  codex-app/        App Server 协议和本地事件读取
  protocol/         用户 API 与 Server Runner 同步契约
transport/
  tsnet/            Go 模块与独立通信进程
web/                Chat、Issue 讨论、通知与只读视图
tests/
  fixtures/         用户输入、App Server 输出与外部事实
  e2e/              少量跨进程主链
docs/               产品设计、专题契约和实现计划
```

Server 和 Runner 的共享依赖限于协议、传输和 Codex 本机适配等边界。Server 以 Issue 及其关联记录保存工作事实与实际派发，Runner 持有本地执行事实和 workspace。看板从 Issue 读取工作状态。Server 采集并保存 PR / CI 对象、版本、状态和证据。发给 Router 的输入在 Server 本地入队，发给工作 Agent 的输入由所属 Runner 同步领取。模块测试就近放置。 [PR 和 CI 事实的采集与投递](design.md#pr-和-ci-事实的采集与投递)

## 消息同步与 Agent 执行

用户写操作在 Server 收件落盘后返回。Server 持久分配可独立执行的任务，Runner 定期取自己的任务，持久接收后本地驱动 Codex 到任务的预期终点。两端可以各自重启和恢复。Runner 心跳上报可用性，消息、执行和事件的原始身份用于去重与对账。HTTP 回执证明已持久接收的范围，原生执行、实际停止和业务应用从各自的证据确认。 [任务分配与投递](server-runner.md#分配与投递)

全量对账是一期同步基线，从 Runner 持久事实生成有确定覆盖范围的对账数据。首期增量 API 按持久回执批量同步新增的完整记录，使用同一套事实和结算逻辑。Chat 的持久同步单位是一条完整消息。App Server 原生增量事件到实时界面的链路留给后续全屏 Agent 干预。 [Server 与 Runner 契约](server-runner.md)

Runner 发起定期同步。外置 tsnet、LAN 和宿主机 `tailscaled` 共用同一套 HTTP 接口和预共享 token 鉴权逻辑。开发用回环传输跑同一套消息契约。同机并行接替的部署验证前驱和后继能同时完成各自的通信。 [通信通道与独立升级](server-runner.md#通信通道与独立升级)

**已确认。** Router 是 Server 的一部分，由 Server 调度本机 Codex App Server 回合。Server 先持久接收 Chat 与 Router 输入，再按需驱动执行。所有工作 Runner 离线时，Router 仍可回复和提交委派。Router 的本机执行故障由 Server 从原输入和回执恢复。各 Runner 按本机持久记录推进已领取的 Issue 工作。 [Router 托管与恢复](chat-routing.md#router-托管与恢复)

一期 Agent 以非沙箱模式使用 Skill 与本机注入的 Tend 工具。Router 的工具在本机 Server 受理，其他角色的工具在本机 Runner 受理后异步交给 Server 应用。操作回执沿原 ID 查询。Codex session 可续用以提高上下文缓存命中，也可从持久输入、Issue 事实和原操作回执新建。 [Agent 工具与本地受理](agent-tools.md)

## 实现顺序与验证范围

1. 先在 devcontainer 跑通 CLI 发消息与 Router 回答，再用固定 fixture 接上 Router 分发、Issue Agent 委派、Work Agent 交付、Issue Agent 上报和 Router 在原 Chat 回报。用一个 Runner slot 核对实际接手和停妥。用全量对账跑通断线回传与丢回复重试，CLI 与纯文本 Web 读取同一 API 结果。 [第一段实现](plans/first-product-slice.md#第一段实现) · [首个产品切片](plans/first-product-slice.md)
2. 一期必须完成 Attention 逐级上报给 Router，再由 Router 在原 Chat 提问和转回答复。第一次试用后接入这条链，再增加 Issue 讨论、Queue 和 Steer。用固定的 PR / CI 输入验证收件路由、订阅、必送和固定窗口合并。 [一期 Attention 验收](plans/first-product-slice.md#一期-attention-验收)
3. 在全量路径基础上，用同一 fixture 核对增量 API 的完整消息边界和结算结果。复用故障 fixture 覆盖同步状态丢失、重启、session 替换、晚到原生终态和写入争用。按范围运行模块测试、协议边界测试和少量端到端用例。
4. 在 devcontainer 验证真实 Codex 和独立进程，在部署环境核对实际配置的通道和预共享 token 鉴权。同机并行接替验证两代独立同步，以及后继在原 workspace 接续同一 Task 的下一次 Run。实际升代时，用前代 Runner 构建产物验证 Server 升级期间原 Task 可以继续并回传，定向验证越窗 Runner 使升级在切换前被拒绝。 [同机 Run 交接](server-runner.md#同机接替时的-run-交接) · [Server 升级兼容窗口](server-runner.md#server-升级兼容窗口)

旧 Tend 的协议 7 离线执行、完整原件同步、精确 PR 等待和按 Issue 合并输入都可以作为实现候选。复用时按一期契约验证其行为。共享的故障 fixture 应验证原任务能继续、用户能看到结果、停妥能释放执行资源，以及原副作用有唯一可追踪的结算。 [离线执行](https://github.com/GhostFlying/tend/pull/391) · [原件同步](https://github.com/GhostFlying/tend/pull/390) · [精确 PR 条件](https://github.com/GhostFlying/tend/pull/406) · [主链故障测试](https://github.com/GhostFlying/tend/pull/307)
