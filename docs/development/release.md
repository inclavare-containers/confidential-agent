# 打包与发布

本文面向维护者，说明如何构建 RPM 与部署 ZIP、发布官方预构建镜像，以及正式版本的发布流程。用户安装说明见 [安装](../install.md)。

## 1. 发布物一览

一个正式 Release（`vX.Y.Z`）包含：

| 发布物 | 说明 |
|---|---|
| `cagent-X.Y.Z-1.al8.x86_64.rpm` | 全功能版：CLI、Guest 二进制、原生工具、模板、一键脚本 |
| `shelter-<ver>.al8.x86_64.rpm` | 配套 Shelter（cagent 通过 `Requires: shelter >= ...` 依赖） |
| `cagent-deploy-X.Y.Z-linux-amd64.zip`、`cagent-deploy-X.Y.Z-darwin-arm64.zip` | 仅部署版 |
| `install-rpm.sh` | RPM 安装器 |
| `image-index.json`、`image-release.json` | 官方预构建镜像的索引与描述（镜像产物本身在 GHCR） |
| cagent SRPM 与离线构建输入 | 源码、vendored Rust 依赖、原生工具归档 |
| `release-manifest.json`、`SHA256SUMS` | 版本、源码提交和全部附件的摘要 |

## 2. 构建 RPM

在 AliLinux 3 打包机上，从源码根目录执行：

```bash
sudo dnf install -y rpm-build cargo rust gcc glibc-devel pkgconf-pkg-config python3.11 curl tar
packaging/build-rpm.sh
```

- 产物位于 `target/rpm/RPMS/x86_64/`，SRPM 位于 `target/rpm/SRPMS/`。
- 源码准备阶段缓存并校验固定版本的 `cosign`、`aliyun`、`ossutil64`、`oras`（清单见 [`packaging/native-tools.tsv`](../../packaging/native-tools.tsv)）。`target/rpm/SOURCES/SHA256SUMS` 记录源码、Cargo vendor 和原生工具归档的摘要；rpmbuild 内部使用 `--locked --offline` 编译和测试。安装后 `/usr/libexec/cagent/SOURCES.tsv` 记录原生工具的版本、URL 和归档摘要（`aliyun`、`oras` 为压缩包摘要）。
- 输出目录同时包含配套 Shelter RPM、`shelter-rpm.json` 和 `SHA256SUMS`。[`hack/shelter-rpm.json`](../../hack/shelter-rpm.json) 记录 GHCR 仓库、源码提交、RPM 摘要和 payload 摘要。缓存中没有该 RPM 时，[`hack/resolve-shelter-rpm.sh`](../../hack/resolve-shelter-rpm.sh) 会用固定版本的 ORAS 按摘要匿名下载并校验。
- `CAGENT_BUILD_JOBS` 控制并行度，默认 `4`。

CI 中的等价流程是 [`packaging/ci-build.sh`](../../packaging/ci-build.sh)（在固定摘要的 AliLinux 3 容器内构建）和 [`packaging/ci-install-test.sh`](../../packaging/ci-install-test.sh)（全新环境安装验证）。

## 3. 构建部署 ZIP

```bash
# cagent（仅部署版）
cargo build --locked --release -p confidential-agent-cli --no-default-features
# Shelter（在 Shelter 源码树中）
cargo build --locked --release --bin shelter --no-default-features --features deploy-only
```

准备目标平台的原生工具和 Terraform provider 后，用 [`packaging/build-deploy-zip.py`](../../packaging/build-deploy-zip.py) 打包（`python3.11 packaging/build-deploy-zip.py --help` 查看参数）。

两种 ZIP 和 RPM 可以用 `packaging/collect-release.py --deploy-bundles DIR` 收集到同一个 Release 目录。只更新部署工具时，`packaging/publish-release.py` 的 `--reuse-image-from TAG` 可以沿用已发布的官方镜像及其原始摘要。

## 4. 发布官方预构建镜像

官方共享镜像由 [`examples/allinone`](../../examples/allinone/README.md) 构建。

```bash
# 在干净的发布提交上构建
python3.11 packaging/produce-release-image.py build WORK_DIR
# 推送到 GHCR，并生成 image-index.json 与 image-release.json
python3.11 packaging/produce-release-image.py push WORK_DIR OUTPUT_DIR [--repository STAGING_REPO]
```

把生成的 `image-index.json`、`image-release.json` 上传到 draft Release。通过 staging 仓库交接时，发布前要把同一个 OCI digest 提升到官方仓库。

沿用已有官方镜像时，在 [`hack/release-image.json`](../../hack/release-image.json) 的 `reuse_image_from` 中写入来源标签（例如 `v0.3.0`）。发布任务会沿用其 OCI 摘要和 `image-release.json`，并把本次工具版本写入新的 `image-index.json`。

## 5. 正式发布流程

[`binary-release.yml`](../../.github/workflows/binary-release.yml) 在固定摘要的 AliLinux 3 容器中执行版本一致性检查、下载测试、RPM 构建和全新安装验证；`vX.Y.Z` 标签通过检查后发布 GitHub Release。

1. **更新版本号**：`Cargo.toml`、`Cargo.lock`、`cagent.spec`、`hack/release-image.json` 和安装脚本中的版本号；编写 `docs/releases/vX.Y.Z.md`（发布任务会读取它作为 Release Notes）；运行 `python3.11 packaging/check-version.py`。
2. **（如需）更新配套 Shelter**：用有写权限的账号，把原样 RPM 作为单个 OCI 文件上传到 `ghcr.io/inclavare-containers/confidential-agent/shelter-rpm`，以 Shelter 的版本和发行号作为标签。核对匿名按摘要下载的字节后，更新 `hack/shelter-rpm.json`。
3. **（如需）更新 Guest 镜像**：按第 4 节构建并推送；否则在 `hack/release-image.json` 中设置 `reuse_image_from`。
4. **上传部署 ZIP**：把已在目标平台验证过的 Linux x86_64 与 macOS arm64 ZIP 上传到同一个 draft Release。
5. **打标签**：推送 `vX.Y.Z` 标签。CI 把两份 ZIP 纳入 Release 清单，核对标签、源码提交、公开 OCI 引用和附件摘要后正式发布。
6. **同步一键入口**：公开一键脚本从 `one-click` 分支下载，发布后把该分支与新版本同步。
