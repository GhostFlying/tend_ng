# 主干 CI 与 Runner 协议窗口验证

## 目标与范围

在 GitHub `main` 推送和手动触发时构建 Rust Server、Runner、CLI 的锁定依赖 release 产物，以及独立的 tsnet 和 Web 产物。沿用 devcontainer 的 Rust、Go、Node 与 pnpm 基线，使用仓库现有构建入口。Go 和 Web 的已有检查先在本地运行，再把稳定通过的检查纳入 CI。工作流使用只读仓库权限。

CI 运行 Go 测试，以及 Web 的类型检查、lint、格式检查和模块测试。Rust job 直接用当次的三个 release 产物运行协议夹具。

按 Server 升代预检契约补全响应。返回目标兼容范围，并在每个阻塞项中返回 Host、Runner 和已登记协议。同步和预检沿用原兼容窗口与退役规则。生产发布、生产升级、版本号、tag 与分支保护均在本次范围之外。

## 夹具与验证

`tests/e2e/protocol_window.py` 接收 release 版 Server、CLI 和 Runner 的路径。每次在独立的临时目录启动真实 Server，在回环地址上用 `runner --once` 登记真实 v1 Runner，经 CLI 核对登记和目标 2 的预检，并直接检查当前同步入口接受 v1、拒绝 v0 和 v2 的声明。

在真实 v1 登记完成后，在临时 Server SQLite 库中模拟 v2、v3 协议登记及 v1 的离线和 drain 状态，通过 CLI 建立替换关系并登记退役。目标 3 的窗口为 v2–v3，未退役 v1 阻塞，建立替换关系后和 drain 后均阻塞，退役后仍可在 Runner 列表查到，且移出预检。目标 4 的窗口为 v3–v4，未退役 v1 和 v2 均阻塞。当前可执行产物使用 v1 协议。v2、v3 登记和目标 2–4 均用于验证预检逻辑，真实跨发布产物验证留待有对应发布产物时进行。

推送前在相同工具链上执行 CI 的构建和检查，并用 release 产物在新临时目录复跑夹具。推送后读回 `origin/main`，按完整提交 SHA 核对 Actions 运行及各 job 的最终结论。
