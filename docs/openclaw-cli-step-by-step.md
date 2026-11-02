# OpenClaw CLI 分步部署示例

本文用一个单实例 OpenClaw 场景串起 `confidential-agent` CLI 的主要命令。与 one-click 路径不同，这里显式执行准备、校验、构建、部署、证明报告、连接和清理步骤。

## 场景

- 部署机：Alibaba Cloud Linux 3。
- 目标服务：`examples/openclaw/openclaw.yaml` 中的 OpenClaw Agent。
- 证明模式：`rekor`，使用 cagent/Shelter 提供的 `cosign` 和 `rekor-cli`；部署机不需要单独安装这两个二进制。
- 云资源：由 Shelter/Terraform 在阿里云创建一台 TDX ECS。

## 1. 准备 CLI 和原生工具

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

> Rekor 模式仍需要 Shelter RPM 提供 `/usr/libexec/shelter/slsa/slsa-generator`；`cosign` 使用 cagent 原生工具目录，`rekor-cli` 复用 Shelter 的私有工具目录，CLI 会为 Shelter 配置 PATH。

## 2. 准备凭证和 OpenClaw 配置

```bash
export ALICLOUD_ACCESS_KEY="<YOUR_ACCESS_KEY>"
export ALICLOUD_SECRET_KEY="<YOUR_SECRET_KEY>"
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"
export GATEWAY_TOKEN="$(openssl rand -hex 20)"
export OPERATOR_CIDR="$(curl -fsSL https://ipinfo.io/ip)/32"

cd examples/openclaw
python3.11 - <<'PY'
import json
import os
from pathlib import Path

path = Path("openclaw.json")
data = json.loads(path.read_text())
data["models"]["providers"]["bailian"]["apiKey"] = os.environ["DASHSCOPE_API_KEY"]
data["gateway"]["auth"]["token"] = os.environ["GATEWAY_TOKEN"]
path.write_text(json.dumps(data, indent=2) + "\n")
PY
```

`GATEWAY_TOKEN` 是 OpenClaw 应用层 `gateway.auth.token`，用于 Web、桌面客户端、TUI 和 WebSocket/API 访问鉴权。它不是 Confidential Agent `cai-gateway` 的 token，也不是 `cai-gateway` client 配置。

## 3. 生成 Rekor 签名 key

```bash
confidential-agent \
  --tools-dir /usr/libexec/cagent/bin \
  key generate-cosign \
  --output-key-prefix ./cosign
```

该命令会生成 `./cosign.key` 和 `./cosign.pub`。它实际在 宿主机上运行 `cosign generate-key-pair`。

## 4. 校验 spec 并配置 operator peering

```bash
confidential-agent spec validate --spec ./openclaw.yaml

confidential-agent peering add \
  --role operator \
  --cidr "$OPERATOR_CIDR" \
  --label ops \
  --note "operator workstation"

confidential-agent peering list
confidential-agent peering show ops
```

`peering` 控制安全组中允许访问控制面、状态接口、connect 端口和 debug SSH 的来源 CIDR。

## 5. 构建可信镜像

```bash
confidential-agent \
  --tools-dir /usr/libexec/cagent/bin \
  build --spec ./openclaw.yaml

confidential-agent image list
confidential-agent status --json
```

`build` 会调用 Shelter 生成 UKI/dm-verity 镜像、SLSA provenance，并通过 宿主机的 `cosign`/`rekor-cli` 完成 Rekor 上传。

## 6. 可选：发布镜像供多次部署复用

```bash
confidential-agent image publish openclaw \
  --spec ./openclaw.yaml \
  --region cn-beijing \
  --no-wait

confidential-agent image publish openclaw \
  --spec ./openclaw.yaml \
  --region cn-beijing

confidential-agent image list
```

`image publish` 会把本地 build 产物上传到 OSS，导入为阿里云自定义镜像，并把发布状态写入本地 service state。第一次用 `--no-wait` 时命令会在导入任务创建后返回，状态通常是 `importing`；后续不带 `--no-wait` 重跑会接续等待同一个导入任务，直到镜像 `available`，并清理临时 OSS 对象。直接不带 `--no-wait` 运行也可以一次性等待完成；默认等待云端导入最多两小时，可用 `CA_IMAGE_IMPORT_TIMEOUT_SEC` 调整。

发布记录按 provider、region、variant、build id 和镜像内容 hash 匹配。后续 `deploy` 只有在这些字段匹配且本地发布状态为 `available` 时才复用 `image_id`，从而跳过 Shelter 的上传和 ImportImage 步骤。

## 7. 部署并注入机密资源

```bash
confidential-agent \
  --tools-dir /usr/libexec/cagent/bin \
  deploy --spec ./openclaw.yaml

confidential-agent status --live
confidential-agent tui
confidential-agent report --include-a2a --json --out ./attestation-report.json
```

`confidential-agent tui` 的远程认证面板展示默认 policy 判定、UKI 实测/期望值和 Rekor provenance；Agents 表与 Service Network 面板展示服务地址，并明确区分 Connect、Mesh 和 MCP 端口，不展示跨信任域 A2A 状态。

如果存在匹配的 published image，`deploy` 会复用该自定义镜像创建云资源；否则会使用本地 build 产物并让 Shelter 执行上传和导入。远程证明通过后，CLI 会注入 `openclaw.json`、磁盘密钥和 mesh/A2A 配置。`report` 会汇总本地状态、Guest daemon 状态、EAR 证明和 Rekor 条目。

## 8. 建立访问通道

```bash
confidential-agent connect start \
  --ready-json ./connect-ready.json \
  --log-file ./connect.log

openclaw tui --url ws://127.0.0.1:18789 --token "$GATEWAY_TOKEN"

confidential-agent connect stop --ready-json ./connect-ready.json
```

浏览器也可以访问 `http://127.0.0.1:18789/openclaw`。

这里的 `connect start` 建立的是管理机到 OpenClaw `service.connect` 的 TNG/RATS-TLS 访问通道，远程证明方向是客户端验证服务端。它不接入 `cai-gateway` client，也不会映射或访问 MCP `mcp_ports`；访问鉴权仍由 OpenClaw 应用层 Gateway Token 完成。

## 9. 可选：接入另一个 AgentCard

```bash
confidential-agent a2a add \
  "http://<PEER_PUBLIC_IP>:8089/.well-known/agent-card.json" \
  --alias peer-openclaw \
  --service openclaw

confidential-agent a2a list
confidential-agent a2a show peer-openclaw
confidential-agent a2a sync --all
confidential-agent peering apply
```

如果对端 AgentCard 启用了 Sigstore keyless 签名，在 `a2a add` 时同时传入 `--signer-issuer` 和 `--signer-subject`。

`a2a add` 只声明本服务出向调用 peer 的 desired state，并生成到 peer `service.connect` 的单向 RA 路径。反向调用需要对端单独配置 `a2a add` 和 peering；A2A 不访问 MCP `mcp_ports`。

## 10. 资源清理

```bash
confidential-agent destroy openclaw
confidential-agent image unpublish openclaw
confidential-agent image prune --dry-run --all
confidential-agent image rm openclaw
confidential-agent forget openclaw
```

`destroy` 会删除已部署的 ECS、网络和安全组等运行资源。对于普通 deploy 路径，Shelter 仍负责清理它自己创建的临时镜像导入资源；对于 `image publish` 创建并记录的 published image，生命周期由 CLI 独立管理，需使用 `image unpublish` 删除对应自定义镜像并清理残留 OSS 对象。`image prune --dry-run --all` 可先审计所有可清理的 published image，确认后去掉 `--dry-run` 执行。`image rm` 只删除本地镜像文件（包含其硬链接），保留构建元数据和云端发布台账。实例已销毁、云镜像已退休后，可以用 `forget` 删除整个本地管理目录。已导出的发布镜像默认受保护；确认要退休对外发布的镜像时才显式使用 `image unpublish --force`。
