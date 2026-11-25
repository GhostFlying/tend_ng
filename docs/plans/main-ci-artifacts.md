# 主干构建产物

## 目标与边界

给 Main CI 已有的 Rust、tsnet 和 Web 构建增加部署下载入口。三个检查通过后，打包当次的 `tend-server`、`tend-runner`、`tend`、`tend-tsnet` 和静态 Web 资源。归档文件名和内置 `REVISION` 记录完整提交 SHA，旁边提供归档 SHA-256 文件。

devbox 和 smallbox 使用 x86_64 Linux 与 glibc 2.36。Rust 构建在 Debian Bookworm 容器内完成，协议窗口夹具直接运行该构建的原件。tsnet 采用 Linux 无 CGO 构建。部署产物沿现有 Server、Runner 和外置 tsnet 入口使用，各进程与持久数据独立升级。 [Server 与 Runner 的契约](../server-runner.md#通信通道与独立升级)

## 获取与并发

对 `main` 推送或在 `main` 手动触发后，可从 [Main CI](https://github.com/GhostFlying/tend_ng/actions/workflows/main-ci.yml) 的成功运行下载 `tend-ng-linux-amd64-<sha>`。同一仓库内构建按原规则并行，每次运行保留各自的证据和归档。 [GitHub Actions 并发](https://docs.github.com/en/actions/concepts/workflows-and-actions/concurrency)

[Main build 预发布](https://github.com/GhostFlying/tend_ng/releases/tag/main-build) 提供最新成功构建的固定页面。只串行发布预发布，先上传 SHA 对应的归档，再推进预发布 tag 与下载说明。同一提交可重复运行。发布只沿提交历史前进，并保留已经上传的归档供按 SHA 查证。发布 job 以 GitHub 的仓库内容写权限更新预发布，构建与打包沿用只读权限。 [GitHub Actions 产物](https://docs.github.com/en/actions/concepts/workflows-and-actions/workflow-artifacts)

下载 `.tar.gz` 和对应校验值后核对 SHA-256。解包得到 `bin/`、`web/` 和 `REVISION`。Server 通过 `--web-dir` 指向解包后的 `web/`。部署继续使用宿主机已有的通信配置、工作目录和独立 App Server，按已部署的 Runner 协议范围执行正常升级预检。

## 验证

在 devcontainer 用固定工具链构建并核对归档结构、权限、校验值及 ELF 依赖。推送后核对 Actions 运行 SHA、可下载的完整归档和 Main build 页的 SHA、资产及校验值。验证发布脚本重复运行和过时成功构建的处理。此次交付以 GitHub 上可下载的文件为准。
