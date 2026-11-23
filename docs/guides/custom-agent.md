# 打包自定义 Agent

本文面向应用开发者：如何把自己的 Agent（或任意 HTTP/MCP 服务）打包成 CA 机密镜像并部署。读完后你会写出一份可用的 AppSpec，并知道常见的坑在哪里。

前置阅读：[核心概念与术语](../concepts.md)。字段的完整定义见 [AppSpec 参考](../reference/appspec.md)。

本地构建需要 AliLinux 3 上的**全功能版**（见 [安装](../install.md)）。

## 1. 先想清楚四件事

| 问题 | 对应 AppSpec 字段 |
|---|---|
| 服务监听哪些端口？哪些要给部署机或外部组织访问，哪些只给同一状态目录下的其他机密服务访问？ | `service.ports`、`service.connect`（其余自动成为 mesh 端口） |
| 用哪个 systemd unit 判断应用已就绪？ | `service.app_service` |
| 镜像里要装什么？ | `build.packages`、`build.files`、`build.scripts` |
| 哪些配置和密钥**不能**进镜像，要在证明后注入？ | `resources` |

经验法则：**镜像只放代码和依赖，凭据和环境相关的配置一律走 `resources`。** 镜像的度量值是公开可审计的，任何写进镜像的东西都应该被视为公开。

## 2. 目录结构

以一个监听 8080 端口、读取 `/etc/my-agent/config.json` 的 HTTP Agent 为例：

```text
my-agent/
├── my-agent.yaml          # AppSpec
├── install-my-agent.sh    # 构建期在镜像内执行的安装脚本
├── files/
│   └── server.py          # 应用代码（也可以在安装脚本里从 git/npm/pip 拉取）
└── secrets/
    └── config.json        # 运行时配置（含 API Key），不进入镜像
```

## 3. 安装脚本

安装脚本在镜像构建的 post-install 阶段以 root 运行，可以访问网络。它要做的事是：安装依赖，并注册一个 systemd unit。

```bash
#!/bin/bash
set -euo pipefail

install -d -m 0755 /opt/my-agent
install -m 0755 /usr/local/share/my-agent/server.py /opt/my-agent/server.py

cat >/etc/systemd/system/my-agent.service <<'EOF'
[Unit]
Description=My confidential agent
After=network-online.target confidential-agentd.service
Wants=network-online.target

[Service]
ExecStart=/usr/bin/python3.11 /opt/my-agent/server.py --config /etc/my-agent/config.json --listen 0.0.0.0:8080
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

systemctl enable my-agent.service
```

要点：

- **配置文件在构建时还不存在**，它在实例启动、远程证明通过后才被注入。守护进程会在写入资源前停止 `app_service`，写完后再启动它。应用启动时要能读到配置，读不到就失败重试（`Restart=always`），不要依赖构建期写入默认值。
- **connect 端口要监听在实例网卡上**（例如 `0.0.0.0`），不能只监听 `127.0.0.1`。守护进程会从实例主网卡地址探测这些端口来判断 `app_ready`。只有 mesh 端口（`connect: []`）的服务可以只监听 `127.0.0.1`。
- 脚本里用到的命令（`curl`、`git`、`tar`、`python3.11` 等）要在 `build.packages` 中声明。`spec validate` 会检查常见遗漏。
- 不要在脚本里调用 `dnf`/`yum` 安装系统包，统一写进 `build.packages`。
- 版本请固定（npm/pip 包版本、git commit），镜像才可复现、度量值才稳定。

## 4. AppSpec

```yaml
schema: confidential-agent/v1

service:
  id: my-agent
  ports: [8080]
  connect: [8080]                    # 允许部署机通过 connect 隧道访问
  app_service: my-agent.service

build:
  image_name: my-agent
  resize: 30G
  packages: [ca-certificates, python3.11]
  files:
    - source: ./files/server.py
      target: /usr/local/share/my-agent/server.py
  scripts:
    - ./install-my-agent.sh          # 部署机上的路径，不是 Guest 路径
  variants:
    release: { enabled: true }
    debug:   { enabled: true }        # 开发期用 debug 镜像方便 SSH 排障

deploy:
  provider: aliyun
  image_variant: debug               # 上线前改为 release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
  disk_gb: 60

attestation:
  tee: tdx
  mode: challenge
  reference_values: sample           # 开发期用 sample；上线改为 rekor，见第 7 节

resources:
  my_agent_config:
    source: ./secrets/config.json
    target: /etc/my-agent/config.json
    mode: "0600"
```

仓库里有几个结构简单、可以直接参考的示例：

| 示例 | 特点 |
|---|---|
| [`examples/mcp`](../../examples/mcp/mcp-demo.yaml) | 最小 MCP Server，只有 mesh 端口并开启 MCP 审计 |
| [`examples/hermes-agent`](../../examples/hermes-agent/README.md) | Python Agent，使用 `mutable` 资源 |
| [`examples/codex`](../../examples/codex/README.md) / [`examples/claude-code`](../../examples/claude-code/README.md) | CLI Agent，经 SSH 使用 |
| [`examples/openclaw`](../../examples/openclaw/openclaw.yaml) | 带 PEP 沙箱和 A2A 的完整示例 |
| [`examples/allinone`](../../examples/allinone/README.md) | 多 Agent 共享镜像，带运行时契约和管理 SSH |

## 5. 构建与部署

```bash
cd my-agent
confidential-agent spec validate --spec my-agent.yaml
confidential-agent peering add --role operator --cidr "$(curl -fsSL https://ipinfo.io/ip)/32" --label ops   # 每个状态目录只需一次
confidential-agent build  --spec my-agent.yaml
confidential-agent deploy --spec my-agent.yaml
confidential-agent status --live
```

`build` 会构建所有启用的变体，`deploy` 部署 `image_variant` 选择的那个。部署 debug 镜像时，CLI 会自动生成 SSH 密钥，并在结束时打印登录命令。

访问服务：

```bash
confidential-agent connect start --service my-agent --ready-json ./ready.json
PORT="$(jq -er '.client_endpoints[0].local_port' ./ready.json)"
curl "http://127.0.0.1:${PORT}/"
```

## 6. 迭代

| 改了什么 | 需要做什么 |
|---|---|
| `resources` 中的文件内容 | `confidential-agent inject --spec my-agent.yaml --target-ip <IP>`，无需重建 |
| 安装脚本、代码、软件包 | 重新 `build`，然后 `destroy` 并重新 `deploy`（新镜像的度量值不同） |
| peering | `peering apply` |

排障时，debug 镜像可以 SSH 登录查看 `journalctl -u my-agent.service` 和 `journalctl -u confidential-agentd.service`。release 镜像需要排障通道时，可开启 `service.ssh: true`（镜像需自带使用 `/opt/data/cai-ssh/` 密钥的 sshd，参见 allinone 示例）。

## 7. 上线前检查

- [ ] `deploy.image_variant: release`，并确认 `variants.release.enabled: true`。release 镜像没有构建期 SSH。
- [ ] `attestation.reference_values: rekor`，填写 `attestation.rekor`（`cosign_key`、`required: true`），用 `confidential-agent key generate-cosign` 生成签名密钥，并妥善保管私钥。
- [ ] 所有凭据都在 `resources` 中，镜像和安装脚本里没有任何密钥。
- [ ] 敏感接口只放在 mesh 端口，`connect` 只放需要从外部访问的端口。
- [ ] 软件版本全部固定。
- [ ] 需要重启后自动恢复时，改用 [Trustee 模式](trustee.md)。
- [ ] operator peering 只包含必要的 CIDR，没有 `0.0.0.0/0`。

## 8. 发布给他人复用

希望别人不构建就能部署你的镜像时：

1. 在 `build.runtime_contract` 中声明镜像的运行时契约（参考 [`examples/allinone/runtime-contract.json`](../../examples/allinone/runtime-contract.json)）；
2. `build` 后执行 `confidential-agent image export <service> --output image-release.json`，把 `image-release.json` 和打印出的 `release_digest` 通过可信渠道交给使用方；
3. 使用方用 `image publish --artifact ...` 导入后，在自己的 AppSpec 中通过 `deploy.image` 引用（见 [AppSpec 参考 · 使用已发布镜像](../reference/appspec.md#42-image使用已发布镜像)）。

官方共享镜像的发布流程见 [打包与发布](../development/release.md)。
