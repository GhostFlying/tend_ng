# GitHub 观测与只读 Issue 看板

Server 使用服务用户的只读 GitHub 权限。外部查询以关联或明确登记的对象为范围。Runner 的 Git 权限用于 workspace 操作。私仓需要由 Server 取得只读访问。 [设计](../design.md#pr-和-ci-事实的采集与投递)

## 第一段

- CLI 与 Work Agent 可以按 URL 将 GitHub PR 登记到 Issue。登记、可重试的操作回执和待观测对象一起持久化。不同 Issue 可以引用同一个 PR，对同一目标的重复登记沿原关联返回。
- Server 的后台观察线程调用本机 GitHub CLI，定期读取已关联 PR 的状态和当前 head 的 CI 汇总。网络读取在事务外。Provider 失败时继续保留链接和最近一次成功观察，界面显示观测暂不可用。
- Web 看板参照旧 Tend 的状态列和紧凑卡片，直接显示关联 PR 链接及已保存的状态。CLI 和 Web 调用同一分页 API。看板仅在 Server SQLite 中读取当页 Issue、当前 Task 和批量关联。运行中的工作和已结束的 Issue 分别呈现。
- 用固定 GitHub CLI 响应、真实 Server 和 Runner 验证显式登记、同一 PR 在两个 Issue 间共享观测、失败读取降级和旧库迁移。浏览器验证卡片链接、移动端排版和查看 Issue 的路径。

## 全局看板与空状态

看板通过 `GET /api/v1/issue-board` 分页读取各对话的顶层 Issue，卡片汇总其子 Issue 的 PR 关联。CLI 用 `tend issue list --scope=roots` 读取同一列表，默认列出全部 Issue，也可用 `tend issue list <chat-id>` 按对话查询。Router 在自己的对话范围内读取工作。空库显示各阶段和空状态。验证不同对话的卡片、按对话查询、分页，以及桌面和窄屏上的列与卡片入口。

## 启动与验证

Server 默认在 PATH 中找 `gh`。使用 Server 服务用户的 `gh` 登录，或从该进程环境提供有只读权限的 `GH_TOKEN`。Runner 继续使用各自的仓库权限。CLI 用 `tend pr link <issue-id> <url>` 写入关联，用 `tend pr list <issue-id>` 和 `tend issue list` 查看。 [GitHub CLI 配置](https://cli.github.com/manual/gh_help_environment)

```sh
cargo build --locked
pnpm --dir web build
python3 tests/e2e/pull_board.py target/debug/tend-server target/debug/tend target/debug/tend-runner --web
```

[桌面看板实拍](images/issue-board-1440.png) · [窄屏看板实拍](images/issue-board-390.png)

## 工作分支补查

- Runner 从托管 worktree 记录实际 GitHub 仓库、推送仓库和工作分支，随开始或停妥事实同步。工作目录和远端凭据留在 Runner。
- CLI 和 Agent 引用目标 Issue 树内的原 Task。Server 持久化待查分支，用仓库和源分支精确查询；只有唯一匹配时建立关联。查询结果以原请求 ID 可查。一期自动补查由工作 Task 的终态和停妥证据入队，每个原 Task 的分支范围只安排一次。
- 用同一个真实进程夹具核对运行期间发起显式补查、终态自动补查、已关联分支去重、多匹配和权限失败。CLI 可读到精确结果，外部查询失败时 Task 仍按实际执行事实结算。

用 `tend pr find <issue-id> <task-id>` 发起补查，用 `tend pr discoveries <issue-id> [operation-id]` 查询请求和每个分支的实际结果。分支查询使用已记录的同名远端工作分支，发布到不同名称时可直接登记 PR URL。 [GitHub 按 head 查询参数](https://docs.github.com/en/rest/pulls/pulls#list-pull-requests)

```sh
python3 tests/e2e/pull_discovery.py target/debug/tend-server target/debug/tend target/debug/tend-runner
python3 tests/e2e/pull_discovery.py target/debug/tend-server target/debug/tend target/debug/tend-runner --already-linked
```

## 外部事件投递

外部事件的订阅、等待与固定窗口投递沿独立的持久投递链路接入。 [PR 关联](../design.md#pr-与-issue-关联) · [任务等待](../task-lifecycle.md)
