# Codebase MR 与 Issue 关联

## 目标与边界

现有 `tend pr link/list/find`、Agent 关联工具、Issue 详情和看板支持 Codebase MR。
按规范 URL 去重，每条外部对象可关联多个 Issue，父卡片汇总按 URL 去重。
保留 GitHub 关联、事实路由、订阅和等待语义。此次仅为 Codebase 提供链接、只读观测与补查，
不添加 Codebase 订阅或 waitfor，也不部署、不修改实际 Issue 或 Codebase MR。

## 实施

- 协议增加 provider，共用规范 URL 和链接读取字段。旧 Runner 工作分支 JSON 默认为 GitHub。
- Server 从 v34、v35 与 v36 升级至 v37，将观测表原地改名并加 provider，保留行、索引和引用外键。
  v35 与 v36 分别保留上游的 Runner 调度和同机交接预算迁移。
- Server 依 provider 分派现有观测与补查。Codebase 通过本机 `bytedcli` 的只读 MR、CI 和窄范围分支查询；
  不传递 Tend 令牌或记录原始错误输出。单次查询有时间与输出上限，失败沿用现有重试周期，
  保存最新错误且不覆盖上一次成功观测。缺少有效 CI 汇总时显示未知。
  本机版本的 `search mr` 不支持 `--state all`，因此使用
  `mr list -R <namespace/project> -H <branch> --state all --page-size 2`，
  必要时翻页。源仓库或匹配唯一性不能确认时不登记。
- 沿用 Runner 确认停妥、Task 终态和一次性自动补查的门槛。用实际 push remote 的源仓库和分支
  核对候选，仅在唯一匹配时写入关联。
- 独立审阅后的修订。CI 查询锁定 MR 当前源提交并关闭 BITS 步骤日志。用仓库 ID、MR 编号与源提交
  核实摘要，即使 CLI 返回空 path 也能确认身份。检查数与总数不一致时保留未知。摘要没有失败或运行项
  时，通过只读 `checks list --commit <sha> --mr <number>` 核对同一 MR 的完整逐项结果。
  每项须已完成且有明确的不失败结论，并且至少有一项明确成功，才显示通过。不用分支级检查或缺结论的条目代替 MR 检查。
  Codebase 源仓库来自 Git 展开 URL 重写后的全部 push URL。多个地址只在全部指向同一 Codebase
  仓库时可用于补查。验证 pushInsteadOf 改写到 fork 和多个不同推送目的地两种情形。
- 页面共用链接文案和状态语义，Codebase 使用 `仓库 !编号`，GitHub 保留 `仓库 #编号`。

## 验证

使用隔离 SQLite、真实 Server/CLI/Runner 和固定只读 CLI fixture，验证显式登记、
幂等和冲突、父卡片去重、权限或超时失败保留事实、未知 CI、唯一或不确定的终态补查、
旧库迁移及 GitHub 关联、补查、外部路由和等待回归。运行 Rust workspace 测试、Web build、
类型检查、Web 测试，并用 Playwright 校验真实页面桌面 1440px、手机 390px，保存最终渲染图。

### 复跑命令

在仓库根目录执行。浏览器验收用本地 Chromium 和项目锁定的 Playwright，
通过实际 HTTP API 登录隔离 Server，不连接真实 Issue。

```bash
cargo test --workspace --locked --offline
cargo test --locked --offline -p tend-server codebase
cargo test --locked --offline -p tend-server legacy_upgrade_preserves_links
cargo build --workspace --locked --offline

pnpm --dir web install --frozen-lockfile
pnpm --dir web typecheck
pnpm --dir web lint
pnpm --dir web format:check
pnpm --dir web test
pnpm --dir web build

bins=(target/debug/tend-server target/debug/tend target/debug/tend-runner)
python3 -B tests/e2e/codebase_pulls.py "${bins[@]}" --screenshots docs/plans/images
python3 -B tests/e2e/pull_board.py "${bins[@]}" --web
python3 -B tests/e2e/pull_discovery.py "${bins[@]}"
python3 -B tests/e2e/pull_discovery.py "${bins[@]}" --already-linked
python3 -B tests/e2e/external_routing.py "${bins[@]}"
python3 -B tests/e2e/external_wait.py "${bins[@]}"
python3 -B tests/e2e/protocol_window.py "${bins[@]}"
python3 -B tests/e2e/discussions.py "${bins[@]}"
```

真实 `bytedcli` 的只读抽查确认仓库内分支过滤可用。候选的 `URL` 可能为空，
因此用核实过的仓库和编号构造规范链接。只读样本也可能不返回源分支或可确认的 CI，
此时不补造状态。自动补查无法核实源仓库或唯一性时不会写入关联。
实测固定源提交且关闭日志的 `checks mr` 返回空 `repository.path`，但仓库 ID 和源提交
均与 `mr get` 一致，检查计数与总数相等。
同一提交的 `checks list` 分别返回分支和 MR 的检查明细，实际成功结论可为 `succeeded`。

### 最终渲染

合成数据同时展示 Codebase 和 GitHub 的同编号链接、未知 CI、上次成功结果、权限失败和运行中 CI。

| 页面 | 桌面 1440px | 手机 390px |
| --- | --- | --- |
| Issue 看板 | [桌面截图](images/codebase-board-1440.png) | [手机截图](images/codebase-board-390.png) |
| Issue 详情 | [桌面截图](images/codebase-issue-1440.png) | [手机截图](images/codebase-issue-390.png) |
