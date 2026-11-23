# 共享预构建镜像（allinone）

本目录定义一个共享 CPU `release` 机密镜像，内置 **OpenClaw、Hermes、Codex、OpenCode、DeepSeek Harness (DSH)、Claude Code、QoderCLI、Pi** 八种 Agent harness。每台部署的 TDX 虚拟机在远程证明通过后接收各自的 `/opt/data/allinone/runtime.json` 与原生配置文件，按实例激活其中一种 harness 及对应运行模式。

基础镜像的虚拟容量为 `30G`，最终 QCOW2 制品使用 zlib 压缩。根文件系统启用 dm-verity 完整性保护，可写增量层由 Cryptpilot / LUKS2 加密。实例系统盘默认 120 GiB，给加密可写层预留空间。`service.ssh: true` 为每个实例生成独立的客户端与主机 Ed25519 密钥并通过远程证明下发，共享镜像内不内置任何 SSH 密钥或模型凭据。本地 `confidential-agent` CLI 不会在宿主机自动安装任何应用客户端。

## 信任模型

镜像交付完整性与运行时远程证明相互独立、分层生效：

1. **镜像交付**。以官方 GitHub Release 的 `image-index.json` 或本机 `image export` 提供的 SHA-256 校验 `image-release.json` 的完整字节。`image publish --artifact` 校验产物 manifest、磁盘摘要及大小。镜像站用于传输产物。
2. **实例启动**。`deploy` 校验镜像描述和运行时契约。实例启动时通过 TDX Quote 和 Rekor 参考值验证后，challenge 或 Trustee 才释放磁盘密钥与实例资源。开发自测可选 `sample` 参考值。

<a id="build-and-publish"></a>

## 构建、导入与多场景复用

镜像可以直接使用官方预构建版本，也可以从本目录自行构建。按已有产物选择路径（下载官方镜像、导入本地产物、复用已有 ECS 镜像、从零构建）的完整步骤见 [手动部署 · 准备云镜像](../../docs/guides/manual-deployment.md#2-准备云镜像)。这里只列出与本镜像相关的要点。

**从源码构建并发布**（需要全功能版）：

```bash
confidential-agent init --stage build --non-interactive --output-dir ~/cagent-work
BUILD_SPEC="$HOME/cagent-work/allinone-builder/build.yaml"
confidential-agent build --spec "$BUILD_SPEC"
confidential-agent image export allinone-builder --output ./image-release.json   # 输出 release_digest
confidential-agent image publish allinone-builder --spec "$BUILD_SPEC" --region cn-beijing --output ./published.json
```

也可以在仓库根目录直接构建本目录的配置：`confidential-agent build --spec examples/allinone/build.yaml`。

**基于镜像创建实例**：每个实例用 `confidential-agent init <app>` 生成配置，`<app>` 为下表中的任一 harness；换一个 `--service-id` 即可在同一镜像上再起一个实例，无需重新构建或导入：

```bash
confidential-agent init codex --non-interactive \
  --image-release ./image-release.json --release-digest "$RELEASE_DIGEST" --image-id "$IMAGE_ID" \
  --service-id codex-01 --region cn-beijing --output-dir ~/cagent-work
confidential-agent deploy --spec ~/cagent-work/codex-01/instance.yaml
```

[`instance.yaml`](instance.yaml) 是 `init` 生成结果的参考模板。

**凭据与上传端点**：阿里云凭据优先读取环境变量（`ALICLOUD_ACCESS_KEY` / `ALICLOUD_SECRET_KEY` 等），其次读取 `aliyun configure` 的 AK 或 StsToken profile（`ALIBABA_CLOUD_PROFILE` / `ALICLOUD_PROFILE` 选择 profile，`ALIBABA_CLOUD_CREDENTIALS_FILE` / `ALICLOUD_SHARED_CREDENTIALS_FILE` 指定凭证文件）。`CA_IMAGE_OSS_ENDPOINT` 默认 `auto`：同地域 ECS 内网可达时走内网 HTTPS，否则走公网。

## 支持的 Harness 与接入模式

| Harness | 支持模式（`--harness-mode`） | 默认端口 | 接入与使用方式 |
| --- | --- | --- | --- |
| `openclaw` | `gateway`（默认）/ `cli` | `18789` | 原生 OpenClaw Gateway（经 `connect` 隧道提供 Web `/openclaw`、WebSocket/TUI，支持 `--enable-dingtalk` 与 A2A）；或经 SSH 运行 CLI |
| `hermes` | `gateway`（默认）/ `cli` | `8642` | 原生 Hermes HTTP API（经 `connect` 隧道提供带 `API_SERVER_KEY` 鉴权的 HTTP 接口）；或经 SSH 运行 CLI |
| `codex` | `app-server`（默认）/ `cli` | `4500` | 原生带能力令牌鉴权的 WebSocket `app-server`（本地 `codex --remote ws://127.0.0.1:<port> --remote-auth-token-env ...` 经 `connect` 接入）；或经 SSH 运行终端 CLI |
| `opencode` | `web`（默认）/ `cli` | `4096` | 原生带 `OPENCODE_SERVER_PASSWORD` 鉴权的 Web 服务（浏览器访问或本地 `opencode attach` 经 `connect` 接入）；或经 SSH 运行 CLI |
| `dsh` | `web`（默认）/ `cli` | `3080` | 原生带鉴权 Web 服务（经 `connect` 映射端口，并通过 `confidential-agent ssh <id> -- cai-agent --web-url --port <本地端口>` 获取带运行期 `?token=` 的登录地址）；或经 SSH 执行 `cai-agent headless '任务'` |
| `claude-code` | `cli` | 无 | 认证管理 SSH：`confidential-agent ssh <id> --tty -- cai-agent` |
| `qodercli` | `cli` | 无 | 认证管理 SSH：`confidential-agent ssh <id> --tty -- cai-agent`，进入后登录 Qoder 账户并使用官方 `/model` 向导 |
| `pi` | `cli` | 无 | 认证管理 SSH：`confidential-agent ssh <id> --tty -- cai-agent` 或 `cai-agent --print '任务'` |

所有网络服务模式的监听端口必须与 `instance.yaml` 中的 `service.ports` 一致；`cli` 模式要求 `service.ports: []`。通过 TNG 建立本地隧道示例：

```bash
confidential-agent connect start --service openclaw-01 --ready-json ./connect-ready.json
# 使用完成后停止本地隧道
confidential-agent connect stop --ready-json ./connect-ready.json
```

通过实例级认证 SSH 检查配置或进入交互会话：

```bash
confidential-agent ssh codex-01 -- cai-agent --check
confidential-agent ssh codex-01 --tty -- cai-agent
```

## 实例级 PEP 控制与远程证明 Skill

共享镜像为全部 8 种 harness 预装 `tdx-remote-attestation` skill 及 `/usr/local/bin/cai-pep` 远程证明辅助工具：

| Harness | Guest 内 Skill 目录 |
| --- | --- |
| `openclaw` | `/opt/data/allinone/home/.openclaw/skills/tdx-remote-attestation` |
| `hermes` | `/opt/data/allinone/hermes/skills/tdx-remote-attestation` |
| `codex` | `/opt/data/allinone/home/.agents/skills/tdx-remote-attestation` |
| `opencode` | `/opt/data/allinone/home/.config/opencode/skills/tdx-remote-attestation` |
| `dsh` | `/opt/data/allinone/home/.dsh/skills/tdx-remote-attestation` |
| `claude-code` | `/opt/data/allinone/home/.claude/skills/tdx-remote-attestation` |
| `qodercli` | `/opt/data/allinone/home/.qoder/skills/tdx-remote-attestation` |
| `pi` | `/opt/data/allinone/home/.pi/agent/skills/tdx-remote-attestation` |

- **OpenClaw 实例级 `--disable-pep`**：共享镜像内置 OpenClaw 执行器补丁、`cai-pep` 插件、Podman 无网只读沙箱以及预编译的 `dingtalk` 与 `cai-a2a` 扩展。OpenClaw 实例默认启动 `cai-pep.service` 强制拦截高危 `exec` 调用；初始化 OpenClaw 实例时若传入 `--disable-pep`（对应 `runtime.json` 中 `"disable_pep": true` 且不启用 `cai-pep` 插件），仅关闭该实例的 `exec` 策略沙箱控制，**TDX 运行时远程证明与 `cai-pep attest collect-and-verify` 证明能力完整保留**。
- 其他 7 种 harness 不挂载 `cai-pep` 命令拦截服务，仅使用 `/usr/local/bin/cai-pep attest collect-and-verify` 执行本机 TDX 证据采集与校验。

## 镜像内运行时配置路径

`/opt/data/allinone/runtime.json` 支持字段 `harness`、可选 `mode`、可选 `port`、可选布尔值 `disable_pep`（仅 `openclaw`）及字符串映射 `environment`。各 harness 在加密可写层下的原生配置位置如下：

| Harness | 加密可写层内的原生配置路径 |
| --- | --- |
| `openclaw` | `/opt/data/allinone/home/.openclaw/openclaw.json`（Gateway 默认端口 `18789`，要求至少 32 字符的 `gateway.auth.token`） |
| `hermes` | `/opt/data/allinone/hermes/config.yaml` 与 `.env`（HTTP API 默认端口 `8642`，要求 `API_SERVER_KEY`） |
| `codex` | `/opt/data/allinone/home/.codex/config.toml`、`OPENAI_API_KEY`；`app-server` 额外要求 `0600` 的 `app-server-token` |
| `opencode` | `/opt/data/allinone/home/.config/opencode/opencode.json`；`web` 模式要求 `OPENCODE_SERVER_PASSWORD` |
| `dsh` | `/opt/data/allinone/home/.dsh/cordis.patch.yml`（`DSH_HOME` 指向该目录） |
| `claude-code` | `/opt/data/allinone/home/.claude/settings.json` 与 `.claude.json` |
| `qodercli` | `/opt/data/allinone/home/.qoder`（自动固定 `general.enableAutoUpdate: false`） |
| `pi` | `/opt/data/allinone/home/.pi/agent/models.json` 与 `settings.json`（`PI_CODING_AGENT_DIR` 指向该目录） |

镜像内软件版本由 [`runtime-contract.json`](runtime-contract.json) 固定，实际安装结果记录在 `/usr/local/share/confidential-agent/allinone-versions.txt`，去重与缓存清理统计记录在 `/usr/local/share/confidential-agent/allinone-compaction.json`。

## 运维与资源清理

- `app_ready` 仅表示 systemd 服务与声明端口就绪，不代表外部付费模型请求已成功；配置检查可执行 `confidential-agent ssh <id> -- cai-agent --check`，排障日志可查看 `confidential-agent ssh <id> -- journalctl -u cai-allinone.service --no-pager`。
- `confidential-agent destroy <service-id>` 仅销毁该实例的 ECS、VPC、安全组等运行资源，保留共享云镜像。
- 确认当前地域不再有任何实例依赖该云镜像后，按构建服务名或 Shelter manifest 摘要执行 `confidential-agent image unpublish <allinone-builder | sha256:manifest摘要> --region cn-beijing`（已导出的保留镜像需显式加 `--force`）清理云镜像与导入快照。
