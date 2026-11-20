# OpenClaw CLI 分步部署示例

本文展示不走 `one-click`、完全通过 `confidential-agent` CLI 显式完成 OpenClaw 准备、镜像导入或构建、部署、远程证明校验、可信连接、A2A 接入与资源清理的完整手动流程。本地 `confidential-agent` CLI 从不在宿主机自动安装任何应用客户端。

## 1. 准备 CLI、原生工具与基础环境变量

在 Alibaba Cloud Linux 3（`x86_64`）部署机上，推荐直接从 [v0.3.1 Release](https://github.com/inclavare-containers/confidential-agent/releases/tag/v0.3.1) 安装已校验的 `cagent` 与配套 `shelter` RPM（详见 [`docs/rpm.md`](rpm.md)）：

```bash
sudo bash install-rpm.sh --version 0.3.1
confidential-agent --version
```

如需从源码编译安装：

```bash
git clone https://github.com/inclavare-containers/confidential-agent.git
cd confidential-agent

cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep
sudo install -m 0755 target/release/confidential-agent /usr/local/bin/confidential-agent
sudo install -m 0755 target/release/confidential-agentd /usr/local/bin/confidential-agentd
sudo install -m 0755 target/release/cai-gateway /usr/local/bin/cai-gateway
sudo install -m 0755 target/release/cai-pep /usr/local/bin/cai-pep

sudo yum install -y python3.11
sudo yum install -y "$(hack/resolve-shelter-rpm.sh)"
sudo packaging/install-native-tools.sh
```

配置阿里云访问凭证、百炼 API Key 与运维出口 CIDR：

```bash
export ALICLOUD_ACCESS_KEY="<YOUR_ACCESS_KEY>"
export ALICLOUD_SECRET_KEY="<YOUR_SECRET_KEY>"
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"
export OPERATOR_CIDR="$(curl -fsSL https://ipinfo.io/ip)/32"

confidential-agent peering add \
  --role operator \
  --cidr "$OPERATOR_CIDR" \
  --label ops \
  --note "operator workstation"
```

## 2. 按已有产物准备云镜像（预构建或自定义构建）

从官方 GitHub Release 或本机 `image export` 获取可信 `release_digest`，用它校验 `image-release.json` 的完整字节。`image publish --artifact` 校验产物与描述一致后导入 ECS。实例启动时独立验证 TDX Quote 和 Rekor 参考值，验证通过后释放密钥和配置。

请根据手头实际持有的条件选择其一：

### 路径 A：下载官方预构建镜像并导入 ECS

下载脚本输出本地产物路径。以下命令以 JSON 读取结果，随后显式发布。

```bash
DOWNLOAD="$(python3.11 /usr/share/cagent/hack/download-release-image.py --output-dir ./prebuilt --format json)"
ARTIFACT="$(jq -er '.artifact' <<<"$DOWNLOAD")"
IMAGE_RELEASE="$(jq -er '.image_release' <<<"$DOWNLOAD")"
RELEASE_DIGEST="$(jq -er '.release_digest' <<<"$DOWNLOAD")"

confidential-agent image publish \
  --artifact "$ARTIFACT" \
  --image-release "$IMAGE_RELEASE" \
  --release-digest "$RELEASE_DIGEST" \
  --region cn-beijing \
  --output ./published.json

IMAGE_ID="$(jq -er '.image_id' ./published.json)"
```

### 路径 B：预构建产物已下载到本地

若本地已有下载或离线拷贝的 Shelter 产物目录（含 `artifact-manifest.json` 与磁盘镜像），使用官方 Release 的可信 `release_digest` 验证并导入 ECS：

```bash
/usr/share/cagent/hack/resolve-release-image.sh --output-dir ./prebuilt
RELEASE_DIGEST="$(jq -er '.release_digest' ./prebuilt/image-index.json)"

confidential-agent image publish \
  --artifact ./local-shelter-artifact \
  --image-release ./prebuilt/image-release.json \
  --release-digest "$RELEASE_DIGEST" \
  --region cn-beijing \
  --output ./published.json

IMAGE_ID="$(jq -er '.image_id' ./published.json)"
IMAGE_RELEASE="./prebuilt/image-release.json"
```

### 路径 C：当前地域已有可用 ECS 自定义镜像 ID

已有当前地域可用的 ECS 镜像 ID 时，采用与该镜像对应的描述和可信摘要。下面以官方预构建镜像为例：

```bash
/usr/share/cagent/hack/resolve-release-image.sh --output-dir ./prebuilt
RELEASE_DIGEST="$(jq -er '.release_digest' ./prebuilt/image-index.json)"
IMAGE_ID="m-xxxxxxxxxxxxxxxxxxxx"
IMAGE_RELEASE="./prebuilt/image-release.json"
```

### 路径 D：从零构建共享镜像（或复用本机已构建产物）

需要修改镜像软件栈时，使用 `init --stage build` 生成 `build.yaml`、构建完整 Shelter 产物并导出无密钥 `image-release.json`（若本机已执行过 `build`，直接从 `image export` 开始即可）：

```bash
confidential-agent init --stage build \
  --non-interactive \
  --reference-values rekor \
  --output-dir ~/cagent-work

confidential-agent build --spec ~/cagent-work/allinone-builder/build.yaml

# 输出 [ca] release_digest=sha256:<64位摘要>
confidential-agent image export allinone-builder --output ./image-release.json
RELEASE_DIGEST="sha256:$(sha256sum ./image-release.json | awk '{print $1}')"

confidential-agent image publish allinone-builder \
  --spec ~/cagent-work/allinone-builder/build.yaml \
  --region cn-beijing \
  --output ./published.json

IMAGE_ID="$(jq -er '.image_id' ./published.json)"
IMAGE_RELEASE="./image-release.json"
```

## 3. 初始化 OpenClaw 实例并部署（支持同镜像多实例复用）

通过 `init openclaw` 生成实例配置目录并部署（未显式传入 `--model` 时默认使用 `qwen4-max`；同一 `--state-dir` 已执行过 `image publish` 时也可省略 `--image-id`）：

```bash
export GATEWAY_TOKEN="$(openssl rand -hex 20)"

confidential-agent init openclaw \
  --non-interactive \
  --image-release "$IMAGE_RELEASE" \
  --release-digest "$RELEASE_DIGEST" \
  --image-id "$IMAGE_ID" \
  --service-id openclaw-01 \
  --gateway-token "$GATEWAY_TOKEN" \
  --region cn-beijing \
  --zone-id cn-beijing-i \
  --instance-type ecs.g9i.xlarge \
  --output-dir ~/cagent-work

confidential-agent spec validate --spec ~/cagent-work/openclaw-01/instance.yaml
confidential-agent deploy --spec ~/cagent-work/openclaw-01/instance.yaml
```

- **可选特性**：初始化时可追加 `--enable-dingtalk`（配合 `DINGTALK_BOT_CLIENT_ID` / `DINGTALK_BOT_CLIENT_SECRET`）启用钉钉机器人；追加实例级 `--disable-pep` 可仅对该实例关闭 OpenClaw `exec` 策略控制，同时保留 TDX 运行时远程证明与 `cai-pep attest`。
- **同镜像再起新实例**：无需再次构建或再次运行 `image publish`，只需把 `--service-id` 换成新的标识（例如 `openclaw-02`，或改跑 `confidential-agent init codex ... --service-id codex-01`），即可基于同一云镜像部署独立实例。

`GATEWAY_TOKEN` 是 OpenClaw 应用层 `gateway.auth.token`，用于 Web、桌面客户端、TUI 和 WebSocket/API 鉴权，不是 `cai-gateway` 的服务间令牌。

## 4. 查看运行状态与远程证明报告

```bash
confidential-agent status --live
confidential-agent tui
confidential-agent report --include-a2a --json --out ./attestation-report.json
```

`confidential-agent tui` 展示默认证明策略判定、UKI 实测/期望值、Rekor provenance 以及服务网络端口；`report` 汇总本地状态、Guest daemon 状态、EAR 证据与 Rekor 条目。

## 5. 建立可信访问通道

```bash
confidential-agent connect start \
  --service openclaw-01 \
  --ready-json ./connect-ready.json \
  --log-file ./connect.log
```

`connect-ready.json` 中的 `client_endpoints` 记录实际映射的本机地址（默认空闲时为 `127.0.0.1:18789`）：

- **浏览器访问**：打开 `http://127.0.0.1:18789/openclaw` 并输入 `GATEWAY_TOKEN`（无需在宿主机安装任何客户端）。
- **OpenClaw TUI（可选）**：如本机已自行安装匹配的 `openclaw` CLI，可执行 `openclaw tui --url ws://127.0.0.1:18789 --token "$GATEWAY_TOKEN"`。

使用完毕后可停止后台隧道：

```bash
confidential-agent connect stop --ready-json ./connect-ready.json
```

## 6. 可选：独立单应用示例 `examples/openclaw/openclaw.yaml`

如果希望直接基于仓库中的独立单应用模板 [`examples/openclaw/openclaw.yaml`](../examples/openclaw/openclaw.yaml) 从源码自定义构建 OpenClaw 专用镜像，可按以下步骤执行：

```bash
cd examples/openclaw
python3.11 - <<'PY'
import json, os
from pathlib import Path
path = Path("openclaw.json")
data = json.loads(path.read_text())
data["models"]["providers"]["bailian"]["apiKey"] = os.environ["DASHSCOPE_API_KEY"]
data["gateway"]["auth"]["token"] = os.environ["GATEWAY_TOKEN"]
path.write_text(json.dumps(data, indent=2) + "\n")
PY

confidential-agent key generate-cosign --output-key-prefix ./cosign
confidential-agent spec validate --spec ./openclaw.yaml
confidential-agent build --spec ./openclaw.yaml
confidential-agent deploy --spec ./openclaw.yaml
```

## 7. 可选：接入另一个 AgentCard（A2A）

```bash
confidential-agent a2a add \
  "http://<PEER_PUBLIC_IP>:8089/.well-known/agent-card.json" \
  --alias peer-openclaw \
  --service openclaw-01

confidential-agent a2a list
confidential-agent a2a show peer-openclaw
confidential-agent a2a sync --all
confidential-agent peering apply
```

若对端 AgentCard 启用了 Sigstore keyless 签名，在 `a2a add` 时同时传入 `--signer-issuer` 和 `--signer-subject`。`a2a add` 仅声明出向单向 RA 调用路径，不暴露 MCP `mcp_ports`。

## 8. 资源清理

```bash
# 销毁实例运行资源（保留共享云镜像）
confidential-agent destroy openclaw-01
confidential-agent forget openclaw-01

# 查看镜像的账号、地域和 ID
confidential-agent image list
```

确认镜像已无消费者后，按构建服务名执行 `confidential-agent image unpublish allinone-builder --region cn-beijing`。通过 `--artifact` 导入的镜像按 `published.json` 中的 manifest 摘要和镜像 ID 退役：

```bash
MANIFEST_DIGEST="$(jq -er '.manifest_digest' ./published.json)"
IMAGE_ID="$(jq -er '.image_id' ./published.json)"
confidential-agent image unpublish "$MANIFEST_DIGEST" --region cn-beijing --image-id "$IMAGE_ID"
```

曾写入导出描述映射的镜像需要 `--force`。
