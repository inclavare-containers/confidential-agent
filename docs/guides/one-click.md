# 一键部署

`one-click/install.sh` 用一条命令完成以下步骤：安装 CLI，准备镜像，创建 TDX 实例，完成远程证明并注入配置，最后在部署机建立访问隧道。默认使用**官方预构建的共享 CPU 镜像**，可部署其中的任意一种 Agent。

本文覆盖 CPU 场景的全部选项。GPU 场景使用独立脚本：

- OpenClaw + 本地 vLLM：`one-click/install-openclaw-vllm.sh`，见 [OpenClaw 最佳实践](openclaw-best-practice/README.md#步骤三部署-openclaw--vllmgpu-tee)
- 加密模型安全推理：`one-click/install-vllm-inference.sh`，见 [安全大模型推理](secure-llm-inference.md)

## 1. 准备

| 项目 | 要求 |
|---|---|
| 部署机 | AliLinux 3 x86_64（全功能版），或其他 Linux x86_64 / macOS arm64（仅部署版，见 [安装](../install.md#1-选择安装形态)）。预构建模式建议可用磁盘 ≥ 40 GB，本地构建 ≥ 80 GB。 |
| 阿里云凭据 | 建议使用专用 RAM 用户，授予 ECS、VPC、OSS 的创建和删除权限（如 `AliyunECSFullAccess`、`AliyunVPCFullAccess`、`AliyunOSSFullAccess`）。首次导入镜像需要账号已存在 `AliyunECSImageImportDefaultRole`。 |
| 模型凭据 | 默认使用百炼 DashScope。也可以改用任意兼容的推理端点，见 [第 4 节](#4-选择-agent-与接入方式)。 |

```bash
export ALICLOUD_ACCESS_KEY="<YOUR_ACCESS_KEY>"
export ALICLOUD_SECRET_KEY="<YOUR_SECRET_KEY>"
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"
```

CLI 也能读取 `aliyun configure` 配置的 profile（用 `ALIBABA_CLOUD_PROFILE` 选择）。凭据只在当前 shell 中使用，不会写入镜像；应用配置和令牌在远程证明通过后才注入实例。

## 2. 下载脚本

```bash
INSTALL_URL="https://raw.githubusercontent.com/inclavare-containers/confidential-agent/one-click/one-click/install.sh"
INSTALL_SCRIPT="$(mktemp)"
curl -fL --connect-timeout 10 --max-time 90 "$INSTALL_URL" -o "$INSTALL_SCRIPT" ||
  curl -fL --connect-timeout 10 --max-time 90 "https://gh-proxy.org/${INSTALL_URL}" -o "$INSTALL_SCRIPT"
```

公开入口放在 `one-click` 分支，它与最新正式版本同步。已安装 RPM 时，同一脚本也位于 `/usr/share/cagent/one-click/install.sh`。

## 3. 部署

**交互式**：依次询问镜像来源、Agent（harness）、接入方式、模型、凭据和允许访问的 CIDR，回车即采用默认值。

```bash
sh "$INSTALL_SCRIPT"
```

**非交互式**：适合自动化，缺少必要参数时直接报错退出。

```bash
sh "$INSTALL_SCRIPT" deploy \
  --non-interactive --yes \
  --region cn-beijing --zone-id cn-beijing-i \
  --instance-type ecs.g9i.xlarge --disk-gb 120 \
  --model qwen3.7-max
```

默认值：

| 参数 | 默认值 |
|---|---|
| `--image-source` | `prebuilt`（官方预构建镜像），可选 `build`（本地构建，需要全功能版） |
| `--harness` / `--access` | `openclaw` / `web+tui` |
| `--model` | `qwen4-max`（也读取 `DASHSCOPE_MODEL`；`--bailian-model` 是别名） |
| `--region` / `--zone-id` | `cn-beijing` / `cn-beijing-i` |
| `--instance-type` | `ecs.g9i.xlarge` |
| `--disk-gb` | `120`（CPU 场景最小值 120） |
| `--reference-values` | `rekor` |
| `--service-id` | 与 harness 同名，例如 `openclaw` |
| `--state-dir` / `--work-dir` | `~/.confidential-agent` / `~/.confidential-agent/one-click` |
| `--allowed-cidr` | 自动探测的部署机公网出口 IPv4 `/32` |

脚本子命令：

| 子命令 | 作用 |
|---|---|
| `deploy`（默认） | 完整部署 |
| `continue` | 部署中断后续跑。跳过已完成的阶段，只补问剩余步骤需要的凭据。有多个未完成任务时加 `--service-id`；使用了自定义状态目录时加原来的 `--state-dir`。 |
| `install-only` | 只安装部署机组件，不部署 |
| `cleanup` | 销毁实例资源（保留已导入的共享镜像）。部署时自定义过 `--harness` 或 `--service-id` 的，清理时也要传入。 |

想改参数但沿用同一服务 ID 时，先执行 `cleanup --service-id <id>` 结束原任务，再重新部署。

完整参数列表：`sh "$INSTALL_SCRIPT" --help`。

## 4. 选择 Agent 与接入方式

共享镜像内置 8 种 Agent（harness）。用 `--harness` 选择，用 `--access` 选择接入方式：

| `--harness` | 默认 `--access`（可选值） | 运行方式与端口 | 推理接口协议 |
|---|---|---|---|
| `openclaw`（默认） | `web+tui`（`web+tui` / `web` / `tui` / `ssh`） | OpenClaw Gateway `:18789`：Web、TUI、钉钉 | OpenAI Chat Completions |
| `hermes` | `http`（`http` / `ssh`） | Hermes HTTP API `:8642`，Bearer `API_SERVER_KEY` 鉴权 | OpenAI 兼容 |
| `codex` | `ws`（`ws` / `ssh`） | Codex `app-server` WebSocket `:4500`，能力令牌鉴权 | OpenAI Responses |
| `opencode` | `web+attach`（`web+attach` / `web` / `attach` / `ssh`） | OpenCode Web / attach `:4096`，密码鉴权 | OpenAI Chat Completions |
| `dsh` | `web`（`web` / `ssh`） | DSH Web `:3080`，令牌鉴权 | OpenAI Chat Completions |
| `claude-code` | `ssh` | 管理 SSH 终端 | Anthropic Messages（默认百炼 `/apps/anthropic`） |
| `qodercli` | `ssh` | 管理 SSH 终端，进入后登录 Qoder 账户 | 使用 Qoder 账户自身配置 |
| `pi` | `ssh` | 管理 SSH 终端，或 `cai-agent --print '任务'` | OpenAI Chat Completions |

- 网络服务类接入（`web`、`http`、`ws` 等）由部署机上的 `connect` 隧道转发。部署机验证实例的远程证明后，再与实例建立加密连接。
- `ssh` 接入使用 CA 为每个实例生成、经远程证明下发的独立 SSH 密钥：`confidential-agent ssh <service-id> --tty -- cai-agent`。
- OpenClaw 跨组织调用由 `cai-a2a` 插件经 TNG 访问对端的 `/v1/responses`。卡片默认声明的 `/a2a` 在 OpenClaw Gateway 上没有处理器，标准 A2A 客户端按卡片调用会失败。见 [A2A 指南](a2a.md#55-应用调用)。

使用非百炼的推理端点：

```bash
export CA_API_KEY="<YOUR_INFERENCE_API_KEY>"
sh "$INSTALL_SCRIPT" deploy --non-interactive --yes \
  --harness hermes --access http \
  --api-base-url https://your-inference.example/v1 \
  --model your-model
```

**本地客户端**：`web+tui`、`tui`（OpenClaw CLI）、`ws`（Codex CLI）、`web+attach`、`attach`（OpenCode CLI）需要在部署机上有对应客户端。交互模式下脚本会单独询问是否安装，默认跳过。非交互模式默认不安装，即使传了 `--yes`；需要时显式传 `--install-host-client`，或传 `--skip-host-client` 跳过询问。

## 5. 选择镜像来源

默认情况下，脚本优先复用当前账号、当前地域已导入的同一镜像。首次使用时，脚本会下载官方产物、校验摘要并导入。其他情况：

| 你手上已有 | 参数 |
|---|---|
| 什么都没有（默认） | 不加参数，即 `--image-source prebuilt` |
| 想从源码构建共享镜像 | `--image-source build`（仅全功能版） |
| 本机已有构建结果 | `--image-source build --skip-build`，复用同一 `--work-dir` 下的构建产物 |
| 本地 Shelter 产物目录 | `--artifact ./local-shelter-artifact --image-release ./image-release.json --release-digest "$RELEASE_DIGEST"` |
| 当前地域已有可用 ECS 镜像 ID | `--image-id m-xxxx`（跳过下载和导入）。使用本机导出的描述文件时，同时传 `--image-release` 与 `--release-digest`。 |
| 同一镜像再起一个实例 | 换一个 `--service-id`，例如 `--harness codex --access ws --service-id codex-02` |

`RELEASE_DIGEST` 是 `image-release.json` 整个文件的 SHA-256，来自官方 Release 的 `image-index.json` 或本机 `image export` 的输出。在装有 QEMU 和 NBD 的 Linux 部署机上，加 `--verify-disk` 还会额外检查本地磁盘结构和启动材料。

## 6. 安全相关选项

**访问来源（CIDR）**：默认只允许部署机公网出口 `/32` 访问实例的管理和业务端口（仅支持 IPv4）。需要从其他网络访问时：

```bash
sh "$INSTALL_SCRIPT" deploy --non-interactive --yes \
  --allowed-cidr 203.0.113.10/32,198.51.100.0/24
```

`--allowed-cidr` 指定的是用户或运维的入口。脚本仍会探测部署机出口，并写入名为 `deployer` 的 peering，保证资源注入、状态检查和隧道可用。**不要在没有其他防护的情况下使用 `0.0.0.0/0`。**

**PEP 策略沙箱（仅 OpenClaw）**：默认启用。`cai-pep` 把 OpenClaw 的 `exec` 工具调用放进无网络、只读的容器，并拒绝访问元数据服务、下载外部代码、反弹 shell 等高危命令。传 `--disable-pep` 可对该实例关闭这项控制；远程证明、机密下发和 `cai-pep attest` 证明能力不受影响。

**钉钉（仅 OpenClaw）**：加 `--enable-dingtalk`，并导出 `DINGTALK_BOT_CLIENT_ID` 和 `DINGTALK_BOT_CLIENT_SECRET`。

**多副本**：网络服务类接入可用 `--replicas N`（N>1）部署一组共享身份、各自独立磁盘的副本，前端为 NLB。`ssh` 类接入不支持多副本，请用不同的 `--service-id` 部署多个独立实例。

## 7. 部署结果

成功后脚本打印部署摘要，例如：

```text
Deployment summary
  state_dir:    /root/.confidential-agent
  work_dir:     /root/.confidential-agent/one-click
  spec:         /root/.confidential-agent/one-click/openclaw/instance.yaml
  service:      openclaw
  harness:      openclaw (gateway, access: web+tui)
  image_source: prebuilt
  image_id:     m-2ze...
  Web: http://127.0.0.1:18789/openclaw/#token=<gateway-token>
  TUI: openclaw tui --url ws://127.0.0.1:18789 --token "<gateway-token>"
  ready_json:   /root/.confidential-agent/one-click/connect.json
  connect:      running (pid 12345, log /root/.confidential-agent/one-click/connect.log)
```

- 实际本地端口以摘要或 `ready_json` 中的 `client_endpoints` 为准。
- OpenClaw Gateway Token 保存在 `<work-dir>/secrets/cpu-<service-id>.token`，默认是 `~/.confidential-agent/one-click/secrets/cpu-openclaw.token`。使用当前实例部署摘要里的令牌访问服务。
- 默认服务 ID 为 `openclaw` 时，隧道文件名为 `connect.json`；其他服务为 `connect-<service-id>.json`。

## 8. 访问 Agent

| Agent | 访问方式 |
|---|---|
| OpenClaw | 浏览器打开 `web` 地址并输入 token；或运行摘要中的 `openclaw tui ...` |
| Hermes | `POST http://127.0.0.1:<port>/v1/chat/completions`，请求头 `Authorization: Bearer <API_SERVER_KEY>` |
| Codex | 运行摘要中的 `CODEX_REMOTE_TOKEN=... codex --remote ws://127.0.0.1:<port> --remote-auth-token-env CODEX_REMOTE_TOKEN` |
| OpenCode | 浏览器用用户名 `opencode` 和摘要中的密码登录，或运行摘要中的 `opencode attach ...` |
| DSH | 浏览器打开摘要中带 `?token=` 的地址 |
| Claude Code / QoderCLI / Pi | `confidential-agent ssh <service-id> --tty -- cai-agent` |

浏览器或客户端在另一台电脑上时，先把部署机本地端口转发过去：

```bash
ssh -N -L 18789:127.0.0.1:18789 root@<部署机公网IP>
```

OpenClaw TUI 首次从新设备连接时，如果提示 `device pairing required`，先用同一 token 登录 Web 控制台，在 **Settings > Devices** 中批准该 `openclaw-tui` 请求。

**验证运行环境**：所有内置 Agent 都预装了 `tdx-remote-attestation` skill。直接在对话中问“这个环境可信吗？”，Agent 会在 Guest 内执行 `cai-pep attest collect-and-verify` 并解释结果。部署机侧的验证方法见 [运维手册](operations.md#查看状态与证明结果)。

## 9. 清理

```bash
sh "$INSTALL_SCRIPT" cleanup                       # 销毁默认实例
sh "$INSTALL_SCRIPT" cleanup --service-id codex-02 # 销毁指定实例
```

`cleanup` 删除实例的 ECS、VPC、安全组等资源，保留已导入的共享云镜像供后续复用。彻底清理镜像、状态目录的步骤见 [运维手册 · 清理资源](operations.md#清理资源)。

## 常见问题

| 现象 | 处理 |
|---|---|
| `peering 'ops' already exists with a different CIDR` | 传入与已有 peering 一致的 `--allowed-cidr`；或在交互模式中确认替换，非交互模式加 `--yes` |
| 官方预构建元数据下载失败 | 检查部署机能否直连 GitHub Release。已有本地产物和可信元数据时，传入 `--artifact`、`--image-release` 和 `--release-digest` |
| challenge 实例重启后服务不可用 | 按 [运维手册](operations.md#challenge-模式重启后恢复) 重新注入资源 |
| 本地隧道端口打不开 | 查看 `~/.confidential-agent/one-click/connect*.log` 和 `confidential-agent status --live`。部署机公网出口变化后需更新 `deployer` peering 并执行 `confidential-agent peering apply`，或直接重跑脚本 |
| 对话没有响应 | 检查 API Key、模型名和账号额度。查看日志：`confidential-agent ssh <service-id> -- journalctl -u cai-allinone.service -n 200 --no-pager` |
| `app_ready` 已就绪但对话失败 | `app_ready` 只表示服务进程和端口就绪，不代表外部模型调用成功。检查配置：`confidential-agent ssh <service-id> -- cai-agent --check` |

更多排错见 [运维手册](operations.md#排错)。
