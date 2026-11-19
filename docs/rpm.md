# confidential-agent RPM 安装与预构建镜像流程

当前支持 Alibaba Cloud Linux 3（`x86_64`）。`confidential-agent`（兼容别名 `cagent`）使用原生宿主机工具，预构建镜像导入、本地构建和云端部署均不需要宿主机安装 Docker 或 Podman；Guest 内应用如启用 PEP，仍使用 Guest 自身的容器沙箱隔离。本地 `confidential-agent` CLI 从不在宿主机自动安装任何应用客户端。

## 从 Release 安装

从 [v0.3.0 GitHub Release](https://github.com/inclavare-containers/confidential-agent/releases/tag/v0.3.0) 下载并运行安装器：

```bash
curl -fsSLo install-rpm.sh https://github.com/inclavare-containers/confidential-agent/releases/download/v0.3.0/install-rpm.sh
sudo --preserve-env=CA_GITHUB_PROXY_URL,CA_GITHUB_PROXY_FIRST bash install-rpm.sh --version 0.3.0
confidential-agent --version
```

安装器会下载并校验 `cagent-0.3.0-1.al8.x86_64.rpm` 与配套 `shelter-0.3.2-4.al8.x86_64.rpm` 的文件 SHA-256 和 RPM payload 摘要，再通过 DNF 一并安装，无需克隆源码或编译 Rust：

- `--download-only --output-dir DIR`：仅下载并校验两个 RPM 及 `release-manifest.json`。
- `--from-dir DIR`：从已下载目录校验并安装本地包（仍通过系统源解析常规 RPM 依赖）。

安装完成后，执行 `/usr/share/cagent/hack/resolve-release-image.sh --output-dir ./prebuilt` 下载镜像索引与描述文件。可用 `--version V` 选择 Release 版本，或用 `--from-dir DIR` 读取已从官方 Release 获取的索引和描述文件。

## 构建 RPM

在打包机安装构建依赖后，从源码根目录执行：

```bash
sudo dnf install -y rpm-build cargo rust gcc glibc-devel pkgconf-pkg-config python3.11 curl tar
packaging/build-rpm.sh
```

产物位于 `target/rpm/RPMS/x86_64/cagent-0.3.0-1.al8.x86_64.rpm`，SRPM 位于 `target/rpm/SRPMS/`。源码准备阶段缓存并校验固定版本的 `cosign`、`aliyun`、`ossutil64` 和 `oras`；`target/rpm/SOURCES/SHA256SUMS` 记录源码、Cargo vendor 和原生工具归档的哈希，rpmbuild 内使用 `--locked --offline` 编译与测试。

输出目录同时包含配套 Shelter RPM（`cagent` 依赖 `shelter >= 0.3.2-4`）、`shelter-rpm.json` 与 `SHA256SUMS`。`hack/shelter-rpm.json` 记录 GHCR 仓库、源码提交、RPM SHA-256 和 payload 摘要。缓存中缺少该 RPM 时，`hack/resolve-shelter-rpm.sh` 获取固定版本的 ORAS，按 RPM SHA-256 匿名下载，并校验包文件和 payload 后写入缓存。`CAGENT_BUILD_JOBS` 控制打包并行度（默认 `4`）。

## 在干净机器手动安装本地 RPM

将 `cagent` 与配套 `shelter` RPM 放在同一目录后安装：

```bash
grep -E '  (cagent-0\.3\.0-1\.al8\.x86_64|shelter-0\.3\.2-3\.al8\.x86_64)\.rpm$' SHA256SUMS | sha256sum -c -
sudo dnf install -y ./shelter-0.3.2-4.al8.x86_64.rpm ./cagent-0.3.0-1.al8.x86_64.rpm
confidential-agent version
rpm -q cagent shelter attestation-challenge-client libtdx-verify trusted-network-gateway
```

如果机器上已安装同版本号的包，需使用 `sudo dnf reinstall -y` 重新安装对应本地 RPM。

## 镜像交付与运行时证明

- **镜像交付**。从官方 GitHub Release 的 `image-index.json` 读取不可变 OCI 引用 `oci_reference` 和可信 `release_digest`，或使用本机 `image export` 输出的摘要。该摘要校验 `image-release.json` 的完整字节。`image publish --artifact` 校验本地产物，确认其 manifest 和磁盘摘要及大小与描述一致。镜像站只负责传输。
- **实例启动**。`deploy` 验证镜像描述和运行时契约，再创建 TDX 实例。通过 TDX Quote 和 Rekor 参考值验证后，challenge 或 Trustee 才释放磁盘密钥与业务资源。开发场景也可使用 `sample` 参考值。

## 按手头已有产物选择手动流程

构建共享镜像用 `confidential-agent init --stage build`，导入 ECS 用 `confidential-agent image publish`，创建应用配置用 `confidential-agent init <app>`。

### 1. 从零构建共享镜像（本地尚无构建产物）

```bash
export PATH=/usr/libexec/cagent/bin:$PATH
aliyun configure

# 生成共享镜像构建目录 ~/cagent-work/allinone-builder（含 build.yaml，无需模型 API Key）
confidential-agent init --stage build --non-interactive --output-dir ~/cagent-work
cd ~/cagent-work/allinone-builder

confidential-agent spec validate --spec build.yaml
confidential-agent build --spec build.yaml

# 导出镜像描述文件（无需签名私钥或云端镜像 ID），记录输出的 release_digest=sha256:...
confidential-agent image export allinone-builder --output ./image-release.json

# 显式上传并导入为目标地域阿里云 ECS 自定义镜像
confidential-agent image publish allinone-builder \
  --spec ./build.yaml \
  --region cn-beijing \
  --output ./published.json
```

### 2. 本机已完成构建（复用已有 Shelter 构建记录）

本机已经执行过 `build` 时，无需重新构建镜像，直接导出描述并导入目标地域 ECS：

```bash
cd ~/cagent-work/allinone-builder
confidential-agent image export allinone-builder --output ./image-release.json
confidential-agent image publish allinone-builder \
  --spec ./build.yaml \
  --region cn-beijing \
  --output ./published.json
```

### 3. 下载并导入官方预构建镜像

在安装 RPM 的机器上执行下载脚本，再运行它输出的 `image publish` 命令。下载先尝试官方 GHCR，传输失败后切换至 1ms，使用相同摘要和本地断点继续传输。

```bash
python3.11 /usr/share/cagent/hack/download-release-image.py --output-dir ./prebuilt --region cn-beijing
```

### 4. 预构建产物已下载到本地（离线传输或镜像加速下载后导入）

如果 Shelter 产物目录（含 `artifact-manifest.json` 和磁盘镜像）已经下载或拷贝到本机，用官方 Release `image-index.json` 中的可信 `release_digest` 校验本地产物并导入 ECS：

```bash
/usr/share/cagent/hack/resolve-release-image.sh --output-dir ./prebuilt
RELEASE_DIGEST="$(jq -er '.release_digest' ./prebuilt/image-index.json)"

confidential-agent image publish \
  --artifact ./local-shelter-artifact \
  --image-release ./prebuilt/image-release.json \
  --release-digest "$RELEASE_DIGEST" \
  --region cn-beijing \
  --output ./published.json
```

`--artifact` 可指向本地 Shelter 产物目录或其中的 `artifact-manifest.json`，直接校验本地文件，省去从 OCI 仓库下载。

### 5. 当前地域已有可用 ECS 自定义镜像 ID（直接初始化并部署）

已有当前地域可用的 ECS 镜像时，使用与该镜像对应的描述、可信摘要和 `image_id`。下面以官方预构建镜像为例。使用本机构建时，将 `IMAGE_RELEASE` 设为本机导出的文件，并采用 `image export` 输出的 `RELEASE_DIGEST`。

```bash
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"
IMAGE_RELEASE="./prebuilt/image-release.json"
RELEASE_DIGEST="$(jq -er '.release_digest' ./prebuilt/image-index.json)"
IMAGE_ID="$(jq -er '.image_id' ./published.json)" # 或填写已知的镜像 ID

confidential-agent init openclaw \
  --non-interactive \
  --image-release "$IMAGE_RELEASE" \
  --release-digest "$RELEASE_DIGEST" \
  --image-id "$IMAGE_ID" \
  --service-id openclaw-01 \
  --region cn-beijing \
  --output-dir ~/cagent-work

confidential-agent peering add --role operator --cidr "<DEPLOYER_PUBLIC_IP>/32" --label deployer
confidential-agent spec validate --spec ~/cagent-work/openclaw-01/instance.yaml
confidential-agent deploy --spec ~/cagent-work/openclaw-01/instance.yaml
```

若该镜像是在同一 `--state-dir` 下通过 `image publish` 导入当前账号和地域的，可省略 `--image-id`，CA 会在部署前按 `manifest_digest`、账号与地域自动解析对应的 `image_id`。未显式传入 `--model`（或 `DASHSCOPE_MODEL`）时，默认模型为 `qwen4-max`。

### 6. 复用同一云镜像创建新实例（相同或不同应用）

云端自定义镜像就绪后，拉起更多实例无需重新构建、重新下载或重复执行 `image publish`；只需指定新的 `--service-id` 运行 `init <app>` 再执行 `deploy`：

```bash
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"

# 复用同一镜像再起一个 Hermes HTTP API 实例
confidential-agent init hermes \
  --non-interactive \
  --image-release "$IMAGE_RELEASE" \
  --release-digest "$RELEASE_DIGEST" \
  --image-id "$IMAGE_ID" \
  --service-id hermes-01 \
  --region cn-beijing \
  --output-dir ~/cagent-work

confidential-agent deploy --spec ~/cagent-work/hermes-01/instance.yaml
```

每个实例拥有独立的 `service.id`、磁盘口令、网关身份、实例 SSH 密钥与业务资源配置。更多共享镜像内部细节见 [allinone 说明](../examples/allinone/README.md#build-and-publish)。

## 原生服务接入、SSH 与生命周期管理

支持基于同一共享预构建镜像初始化的应用及接入方式如下：

- **原生网络服务（通过 `confidential-agent connect start` 建立本地 RATS-TLS 隧道访问）**：
  - **OpenClaw**：Gateway 服务（Web `/openclaw`、WebSocket/TUI，支持 `--enable-dingtalk` 与 A2A）。传入 `--disable-pep` 可按实例关闭 `exec` 策略控制，同时保留 TDX 运行时证明与 `cai-pep attest`。
  - **Hermes**：带鉴权的 HTTP API 服务（默认端口 `8642`）。
  - **Codex**：带能力令牌鉴权的 WebSocket `app-server` 服务（默认端口 `4500`，也可切为 SSH CLI）。
  - **OpenCode**：带密码鉴权的 Web / attach 服务（默认端口 `4096`，也可切为 SSH CLI）。
  - **DSH**：带鉴权的 Web 服务（默认端口 `3080`，通过 `confidential-agent ssh <id> -- cai-agent --web-url --port <本地端口>` 获取带登录 token 的地址；也可通过 SSH 运行 `cai-agent headless`）。
- **SSH 终端接入（通过远程证明下发实例级密钥）**：
  - **Claude Code**、**QoderCLI**（进入会话后完成 Qoder 登录）、**Pi**：通过 `confidential-agent ssh <service-id> --tty -- cai-agent` 使用。

常用连接、巡检与销毁命令：

```bash
confidential-agent status --live
confidential-agent report --json

# 原生网络服务建立与停止本地隧道（多条并发隧道请指定不同的 --ready-json 与 --log-file）
confidential-agent connect start --service openclaw-01 --ready-json connect.json --log-file connect.log
confidential-agent connect stop --ready-json connect.json

# 实例级管理 SSH 检查与交互进入
confidential-agent ssh codex-01 -- cai-agent --check
confidential-agent ssh codex-01 --tty -- cai-agent

# 销毁单个实例（保留共享云镜像）
confidential-agent destroy openclaw-01
```

网络服务支持 `--replicas N` 部署共享身份的负载均衡副本。CPU 网络服务使用 NLB，`vllm-inference` 使用 ALB + OHTTP。CLI 模式通过不同的 `--service-id` 创建独立实例。确认无消费者依赖镜像后，用 `image unpublish` 按构建服务名或产物的 `manifest_digest` 退役云镜像与自有导入快照。曾写入导出描述映射的镜像需加 `--force`。

## 目录说明

| 位置 | 用途 |
|---|---|
| `./confidential-agent-init/allinone-builder/` | `init --stage build` 默认生成的共享镜像构建目录（含 `build.yaml`、脚本与 `NEXT_STEPS.md`），可用 `--output-dir` 修改 |
| `./confidential-agent-init/<service-id>/` | `init <app>` 生成的实例部署目录（含 `instance.yaml`、`image-release.json`、`secrets/` 与 `NEXT_STEPS.md`） |
| `~/.confidential-agent/` | 默认 CA state 目录，可通过全局 `--state-dir` 指定一致的绝对路径 |
| `<state>/services/<service-id>/` | 构建或实例的本地状态、清单、密钥与 Shelter/Terraform 工作目录 |
| `<state>/image-publications/` | `image publish` 按 `manifest_digest`、阿里云账号与地域记录的云镜像发布台账 |
| `/usr/share/cagent/` | RPM 安装的模板资产、`one-click/` 脚本、`hack/` 元数据解析器与 `software-versions.json` |

## GitHub 下载与大陆网络

安装器从官方 GitHub Release 获取 RPM 校验清单。从镜像站下载的 RPM 须匹配清单中的 SHA-256 和 RPM payload 摘要。下列设置控制支持代理的下载步骤：

```bash
# 默认：代理优先，失败回退官方
export CA_GITHUB_PROXY_URL="https://gh-proxy.org/"
export CA_GITHUB_PROXY_FIRST=1

# 海外或可直连环境：官方优先，失败回退代理
export CA_GITHUB_PROXY_FIRST=0

# 完全禁用代理
export CA_GITHUB_PROXY_URL=""
```

## 维护者发布流程

`.github/workflows/binary-release.yml` 在固定摘要的 Alinux3 容器内执行版本一致性检查、下载测试、RPM 构建与全新安装验证；`vX.Y.Z` 标签通过检查后发布 GitHub Release。

1. 更新 `Cargo.toml`、`Cargo.lock`、`cagent.spec`、`hack/release-image.json` 与安装脚本中的版本号，编写 `docs/releases/vX.Y.Z.md`，运行 `python3.11 packaging/check-version.py`。
2. 如配套 Shelter 更新，用有包写权限的账号将原样 RPM 作为一个 OCI 文件上传到 cagent 的公开 GHCR 包 `ghcr.io/inclavare-containers/confidential-agent/shelter-rpm`，用 Shelter 的版本和发行号作为标签。核对匿名按 RPM SHA-256 下载的字节后，更新 `hack/shelter-rpm.json`。正式 cagent Release 随附对应的 Shelter RPM。
3. 在安装新 RPM 的构建机上，从干净的发布提交执行 `python3.11 packaging/produce-release-image.py build WORK_DIR`。执行 `python3.11 packaging/produce-release-image.py push WORK_DIR OUTPUT_DIR` 将已有产物推送至 GHCR 并生成 `image-index.json`、`image-release.json`，然后将两份元数据上传至 draft Release。若通过 staging 仓库交接，给 `push` 传入 `--repository REPOSITORY`，并在发布前将相同 OCI digest 提升到官方仓库。
4. 推送 `vX.Y.Z` 标签，CI 校验标签、源码提交、Rekor 元数据、公开匿名 GHCR 可拉取性及全部附件 `SHA256SUMS` 一致后正式发布 Release。

`/usr/libexec/cagent/SOURCES.tsv` 记录固定原生工具（`cosign`、`aliyun`、`ossutil64`、`oras`）的版本、URL 与下载归档 SHA-256（其中 `aliyun` 与 `oras` 为压缩包摘要）；RPM 自身记录最终安装文件的摘要。
