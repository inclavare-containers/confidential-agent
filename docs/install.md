# 安装

本文说明如何在**部署机**上安装 `confidential-agent` CLI（兼容别名 `cagent`）及其依赖。使用 [一键部署](guides/one-click.md) 时脚本会自动完成安装，一般不需要手动执行本文步骤。

## 1. 选择安装形态

CA 提供两种安装形态：

| 形态 | 能做什么 | 支持的部署机 |
|---|---|---|
| **全功能版（`full`，RPM）** | 本地构建镜像、导入镜像、部署与运维 | Alibaba Cloud Linux 3（AliLinux 3）x86_64 |
| **仅部署版（`deploy-only`，ZIP）** | 导入官方预构建镜像、部署与运维；**不能本地构建镜像** | Linux x86_64（Ubuntu、Debian、Rocky Linux 9 等 RHEL 9 兼容发行版，以及 AliLinux 3）；macOS Apple Silicon（arm64） |

一键脚本的默认选择：

- AliLinux 3 x86_64 默认安装全功能版。部署机只负责部署、不想装 mkosi、QEMU 等构建依赖时，可加 `--edition deploy-only`。
- 其他平台只能使用仅部署版。
- `--image-source build`（本地构建镜像）需要全功能版。

同一台机器上，同一用户使用同版本的 RPM 或 ZIP，默认共用状态目录 `~/.confidential-agent`，切换安装形态后仍可管理已有实例。需要隔离的实验环境时，给所有命令传同一个 `--state-dir`。

## 2. 安装全功能版（AliLinux 3）

从 [v0.4.0 Release](https://github.com/inclavare-containers/confidential-agent/releases/tag/v0.4.0) 下载安装器并运行：

```bash
curl -fsSLo install-rpm.sh \
  https://github.com/inclavare-containers/confidential-agent/releases/download/v0.4.0/install-rpm.sh
sudo --preserve-env=CA_GITHUB_PROXY_URL,CA_GITHUB_PROXY_FIRST bash install-rpm.sh --version 0.4.0
confidential-agent --version
```

安装器会做以下几件事：

1. 从官方 Release 的 `release-manifest.json` 读取期望的 SHA-256；
2. 下载 `cagent-0.4.0-1.al8.x86_64.rpm` 和配套的 `shelter-0.3.2-6.al8.x86_64.rpm`，同时校验文件摘要和 RPM payload 摘要；
3. 通过 DNF 一起安装，并从 AliLinux 3 软件源解析其他依赖（例如 TNG `2.9.2-1.al8`）。

无需克隆源码或编译 Rust。其他用法：

| 参数 | 作用 |
|---|---|
| `--download-only --output-dir DIR` | 只下载并校验两个 RPM 和 `release-manifest.json`，适合离线搬运 |
| `--from-dir DIR` | 从已下载的目录校验并安装 |

手动安装本地 RPM（例如离线环境）：

```bash
grep -E '  (cagent-0\.4\.0-1\.al8\.x86_64|shelter-0\.3\.2-6\.al8\.x86_64)\.rpm$' SHA256SUMS | sha256sum -c -
sudo dnf install -y ./shelter-0.3.2-6.al8.x86_64.rpm ./cagent-0.4.0-1.al8.x86_64.rpm
rpm -q cagent shelter attestation-challenge-client libtdx-verify trusted-network-gateway
```

已装过同版本时用 `sudo dnf reinstall -y`。

## 3. 安装仅部署版（ZIP）

1. 从同一 Release 下载 `cagent-deploy-0.4.0-linux-amd64.zip` 或 `cagent-deploy-0.4.0-darwin-arm64.zip`；
2. 用 Release 中的 `SHA256SUMS` 校验后解压；
3. 在解压目录运行 `./one-click.sh` 进行引导式部署，或运行 `./bin/cagent --help` 分步操作。也可以把 `bin` 目录加入 `PATH`。

ZIP 已包含原生部署工具和 Terraform provider，解压即可运行 CLI，不需要 Python。

通过公开一键脚本加 `--edition deploy-only` 时，ZIP 会被安装到 `${XDG_DATA_HOME:-$HOME/.local/share}/cagent/releases/<版本>/`，供后续复用。

一键脚本在运行镜像下载器或服务探针时需要 Python 3.11 及以上。本机没有合适的解释器时，脚本会下载固定版本的 Python，校验 SHA-256 后安装到 `${XDG_DATA_HOME:-$HOME/.local/share}/cagent/one-click/python/`，不需要 sudo 或 Homebrew。传入 `--skip-deps` 时只使用本机已有的解释器。

## 4. 从源码安装（开发者）

```bash
git clone https://github.com/inclavare-containers/confidential-agent.git
cd confidential-agent

cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep
for bin in confidential-agent confidential-agentd cai-gateway cai-pep; do
  sudo install -m 0755 "target/release/$bin" "/usr/local/bin/$bin"
done

sudo yum install -y python3.11
sudo yum install -y "$(hack/resolve-shelter-rpm.sh)"   # 下载并校验配套 Shelter RPM，输出其本地路径
sudo packaging/install-native-tools.sh                 # cosign、aliyun、ossutil、oras 等原生工具
```

仓库示例的 AppSpec 引用 RPM 的安装路径 `/usr/bin/cai-pep`。从源码安装时，请相应修改示例中的 `build.files[].source`，或把 `cai-pep` 另装一份到 `/usr/bin/`。

更多开发环境说明见 [开发者指南](development/README.md)。

## 5. 宿主机依赖一览

CLI 直接调用部署机上的原生工具，**不需要 Docker 或 Podman**（显式使用 `build.container.image` 的构建除外）。原生工具默认从 `/usr/libexec/cagent/bin` 查找（可用 `--tools-dir` 或 `CA_TOOLS_DIR` 修改），找不到时再查 `PATH`。

| 组件 | 来源 | 用途 |
|---|---|---|
| `confidential-agent` / `cagent` | cagent RPM 或 ZIP | 部署机 CLI |
| `confidential-agentd`、`cai-gateway`、`cai-pep` | cagent RPM | 构建时放入 Guest 镜像，不在部署机上运行 |
| `attestation-challenge-client`、DCAP/TDX 验证库 | RPM 依赖 | 远程证明、资源注入、证明报告 |
| TNG 2.9.2 | RPM 依赖 | `connect` 隧道 |
| cosign 3.0.6 | cagent 私有工具目录 | Rekor 签名、生成签名密钥、AgentCard 签名 |
| aliyun 3.0.232、ossutil 1.7.18 | cagent 私有工具目录 | 镜像上传与导入、ALB 查询 |
| oras 1.3.0 | cagent 私有工具目录 | 按 `@sha256` 拉取或推送 OCI 镜像产物 |
| rekor-cli | Shelter RPM（`/usr/libexec/shelter/bin/rekor-cli`） | 参考值上传与审计 |
| Shelter、Terraform 及 provider | Shelter RPM | 镜像构建、产物校验、云资源生命周期 |
| mkosi、QEMU、LVM、cryptsetup 等 | Shelter RPM 依赖 | 磁盘镜像构建与 `--verify-disk` 检查（仅全功能版） |
| OpenSSH、jq、bash、coreutils、CA 证书 | 系统包 | SSH、脚本 |
| Python 3.11+ | RPM 通过 DNF 安装；ZIP 由一键脚本按需安装到用户目录 | 发布元数据解析、预构建镜像下载、部分就绪探针 |

**应用客户端不会被自动安装。** Node.js、Python 应用包、CUDA、vLLM 等都在 Guest 镜像内准备。一键部署选择了需要本地客户端的接入方式（OpenClaw TUI、Codex 远程 CLI、OpenCode attach）时，脚本会单独询问是否安装，默认跳过；非交互模式需显式传 `--install-host-client`。仅部署版会把客户端和缺少的 Node.js 装到 `${XDG_DATA_HOME:-$HOME/.local/share}/cagent/one-click/` 并打印需要加入的 `PATH`。

安全相关的实现细节：凭据通过进程环境或用后即删的 `0600` 临时文件传给原生工具，不出现在命令行参数中；后台 TNG 日志权限为 `0600`，CLI 通过 PID、启动时间和系统启动标识确认进程身份后才发送信号。

## 6. 安装后目录

| 位置 | 内容 |
|---|---|
| `/usr/bin/confidential-agent`、`/usr/bin/cagent` | CLI |
| `/usr/libexec/cagent/bin/` | 固定版本的原生工具；`/usr/libexec/cagent/SOURCES.tsv` 记录其版本、URL 和归档 SHA-256 |
| `/usr/share/cagent/` | 示例模板、`one-click/` 脚本、`hack/` 下载与校验脚本、`software-versions.json` |
| `~/.confidential-agent/` | 默认状态目录（见 [运维手册](guides/operations.md#本地目录与状态)） |

## 7. 网络与 GitHub 代理

GitHub 下载顺序和校验方式取决于下载内容。下列环境变量控制安装器、引导流程中支持回退的下载，例如 RPM 和部署 ZIP。预构建镜像元数据直接从 GitHub Release 获取。CPU 一键指南中的首次脚本下载用独立的 `curl` 命令先尝试直连。

```bash
# 默认值：代理优先
export CA_GITHUB_PROXY_URL="https://gh-proxy.org/"
export CA_GITHUB_PROXY_FIRST=1

# 海外或可直连：官方优先，失败再走代理
export CA_GITHUB_PROXY_FIRST=0

# 完全禁用代理
export CA_GITHUB_PROXY_URL=""
```

**代理是第三方服务，只解决网络可达性，不提供可信保证。** 各下载步骤经过代理时的校验情况：

| 下载内容 | 校验方式 | 经代理时是否仍可信 |
|---|---|---|
| RPM（`install-rpm.sh` 安装） | `install-rpm.sh` 与 `release-manifest.json` 始终直连 GitHub 获取，RPM 的文件摘要和 payload 摘要按该清单校验 | 是 |
| 预构建镜像产物 | 按官方 Release 中的可信摘要校验 | 是 |
| 一键脚本本身（`install.sh` 等）及其引导脚本 | 无独立校验 | 否，代理可以篡改脚本 |
| 部署 ZIP（一键脚本 `deploy-only` 路径） | 按 `SHA256SUMS` 校验，但 `SHA256SUMS` 与 ZIP 走同一下载通道 | 否，代理可以同时替换两者 |

生产环境建议：能直连 GitHub 时设置 `CA_GITHUB_PROXY_FIRST=0`（或 `CA_GITHUB_PROXY_URL=""`）；部署 ZIP 从官方 Release 页面手动下载，并用直连获取的 `SHA256SUMS` 校验。

预构建镜像产物从 GHCR 下载：先直连 `ghcr.io`，传输失败后切换到镜像站 `ghcr.1ms.run`，使用相同摘要并支持断点续传。镜像站只负责传输，不参与信任判断。

## 下一步

- 用一键脚本部署：[一键部署](guides/one-click.md)
- 手动逐步部署：[手动部署](guides/manual-deployment.md)
