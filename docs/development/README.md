# 开发者指南

本文面向 CA 本身的贡献者：如何构建、测试、定位代码，以及修改文档时的约定。先读 [架构设计](../design/architecture.md) 会更容易理解代码组织。

## 1. 开发环境

- **Rust**：workspace 最低支持 Rust 1.75（见根 [`Cargo.toml`](../../Cargo.toml) 的 `rust-version`），依赖版本全部用 `=` 固定，提交时保持 `Cargo.lock` 同步。
- **完整功能开发**：建议使用 Alibaba Cloud Linux 3 x86_64，本地构建镜像依赖 Shelter RPM（mkosi、QEMU 等）。
- **只改 CLI 的部署能力**：任意 Linux x86_64 或 macOS arm64 即可，使用 `--no-default-features` 编译仅部署版。
- **Python 3.11**：打包脚本、一键脚本辅助工具和 E2E 测试使用。

```bash
git clone https://github.com/inclavare-containers/confidential-agent.git
cd confidential-agent
cargo build --workspace                       # 调试构建全部 crate
cargo build --release -p confidential-agent-cli --no-default-features   # 仅部署版 CLI
```

把源码构建的 CA 安装为部署机工具、并装好 Shelter 与原生工具，见 [安装 · 从源码安装](../install.md#4-从源码安装开发者)。

## 2. 代码导航

| 想改的东西 | 从这里开始 |
|---|---|
| AppSpec 字段、默认值与校验 | [`core/src/spec.rs`](../../core/src/spec.rs)（同时更新 [AppSpec 参考](../reference/appspec.md) 和 [`cli/src/app/self_describe.rs`](../../cli/src/app/self_describe.rs) 中的内置说明） |
| 跨进程 schema（bootstrap、mesh bundle、status 等） | [`core/src/schema.rs`](../../core/src/schema.rs)、[`core/src/mesh.rs`](../../core/src/mesh.rs) |
| CLI 参数定义 | [`cli/src/cli.rs`](../../cli/src/cli.rs) |
| CLI 子命令实现 | [`cli/src/app/commands.rs`](../../cli/src/app/commands.rs)、[`cli/src/app/workflows.rs`](../../cli/src/app/workflows.rs) |
| `init` 生成的实例与构建配置 | [`cli/src/app/init.rs`](../../cli/src/app/init.rs)、[`cli/src/app/init/staged.rs`](../../cli/src/app/init/staged.rs) |
| 镜像导出、发布、退役 | [`cli/src/app/images.rs`](../../cli/src/app/images.rs)、[`cli/src/app/publish.rs`](../../cli/src/app/publish.rs)、[`cli/src/app/image_runtime.rs`](../../cli/src/app/image_runtime.rs) |
| 原生工具调用（attestation-challenge-client、cosign、aliyun 等） | [`cli/src/app/tools.rs`](../../cli/src/app/tools.rs)、[`cli/src/app/native_process.rs`](../../cli/src/app/native_process.rs) |
| Guest 资产、systemd unit、CryptPilot 配置 | [`cli/src/app.rs`](../../cli/src/app.rs)、[`cli/src/app/guest_assets.rs`](../../cli/src/app/guest_assets.rs) |
| Trustee 同步与策略 | [`cli/src/app/trustee.rs`](../../cli/src/app/trustee.rs)、[`core/src/trustee.rs`](../../core/src/trustee.rs)、[`daemon/src/trustee.rs`](../../daemon/src/trustee.rs) |
| A2A / AgentCard | [`core/src/agent_card.rs`](../../core/src/agent_card.rs)、[`core/src/a2a.rs`](../../core/src/a2a.rs)、[`core/src/agent_card_signing.rs`](../../core/src/agent_card_signing.rs) |
| peering 与安全组 | [`core/src/peerings.rs`](../../core/src/peerings.rs)、[`shelter/src/lib.rs`](../../shelter/src/lib.rs)（`security_group_rules`） |
| Shelter 配置渲染 | [`shelter/src/lib.rs`](../../shelter/src/lib.rs) |
| Guest 守护进程 | [`daemon/src/app.rs`](../../daemon/src/app.rs) |
| MCP 网关 | [`gateway/src/lib.rs`](../../gateway/src/lib.rs) |
| PEP 沙箱 | [`cai-pep/src/main.rs`](../../cai-pep/src/main.rs) |
| TUI | [`cli/src/app/tui.rs`](../../cli/src/app/tui.rs) |
| 一键部署脚本 | [`one-click/lib/`](../../one-click/lib/)（入口 `main.sh`，参数默认值在 `init_defaults` / `apply_target_defaults`） |
| 软件版本固定 | [`software-versions.json`](../../software-versions.json)（Guest 内 Agent、Node.js、默认模型等） |

## 3. 测试

CI 定义在 [`.github/workflows/binary-release.yml`](../../.github/workflows/binary-release.yml)。提交前至少运行与改动相关的本地测试：

| 范围 | 命令 |
|---|---|
| Rust 单元与集成测试 | `cargo test --locked --workspace` |
| 版本号一致性 | `python3.11 packaging/check-version.py` |
| Shell 语法 | `for s in packaging/*.sh hack/*.sh one-click/lib/*.sh common/*.sh; do bash -n "$s"; done` |
| 打包脚本 | `python3.11 -m unittest discover -s packaging/tests -v` |
| 一键脚本 | `cargo build --locked -p confidential-agent-cli -p cai-pep && for t in one-click/tests/*.sh; do bash "$t"; done` |
| E2E 本地部分（无需云资源） | `python3.11 -m unittest discover -s tools/e2e/tests -v`，以及 `tools/e2e/tests/*.sh` |
| 覆盖率 | `hack/coverage.sh`（需要 `cargo-tarpaulin`，输出到 `.tmp/coverage/`） |

**云端端到端测试**会创建真实的阿里云资源并产生费用，需要阿里云和百炼凭据。入口是 `tools/e2e/run.sh <case>`，用例、环境变量和验收矩阵见 [`tools/e2e/README.md`](../../tools/e2e/README.md)。示例：

```bash
export ALICLOUD_ACCESS_KEY=... ALICLOUD_SECRET_KEY=... DASHSCOPE_API_KEY=...
tools/e2e/run.sh openclaw-bailian
```

E2E 脚本不会自行清除代理变量。如果开发机需要代理访问外部模型服务，但构建（mkosi/DNF）和云部署必须直连，请在外层用 `env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy` 包装命令。

## 4. 开发约定

- **安全默认值优先**：新增能力默认 fail-closed；涉及机密的数据不进入镜像、日志或命令行参数。
- **AppSpec 严格模式**：新字段要加到对应结构体（`deny_unknown_fields`），补充校验、单元测试和文档。删除字段时给出明确的报错和迁移说明。
- **schema 版本**：改变跨进程数据结构（bootstrap、mesh bundle、status 等）时，评估是否需要提升版本号，并考虑新旧 Guest / CLI 混用的情况（参考运行时契约中的 capability 机制）。
- **版本固定**：Guest 内软件版本放在 `software-versions.json`，原生工具放在 [`packaging/native-tools.tsv`](../../packaging/native-tools.tsv)，下载内容都要有摘要校验。
- **提交前**：建议运行 `cargo fmt`，并运行上表中与改动相关的测试。

## 5. 文档约定

文档目录结构：

```text
docs/
├── README.md        文档导航（新增文档时登记）
├── concepts.md      核心概念与术语（新术语先在这里定义）
├── install.md       安装
├── guides/          面向任务的使用指南
├── reference/       字段与命令参考（必须与代码一致）
├── design/          架构与设计说明（面向贡献者）
├── development/     开发、测试、发布
└── releases/        版本说明（发布流程会读取 vX.Y.Z.md）
```

写作约定：

- 用户文档使用中文；示例目录的 README 可使用英文。
- 术语保持一致：**部署机**（运行 CLI 的机器）、**实例 / Guest**（TDX 虚拟机）、**状态目录**（`--state-dir`）、**服务**（`service.id`）、**镜像变体**（release / debug）。首次出现的概念链接到 [concepts.md](../concepts.md)。
- 默认值、参数名、端口号、路径必须以代码为准，改动代码时同步更新 `reference/` 下的文档和 `self_describe.rs` 中的内置说明。
- 同一操作只在一处详细描述，其他地方链接过去。例如“按已有产物选择镜像路径”只在 [手动部署](../guides/manual-deployment.md) 中详细展开。
- 命令示例应可直接复制执行：占位符用 `<尖括号>`，并说明来源。
- 说明“能保证什么”的同时说明“不能保证什么”，例如 `app_ready` 不代表模型调用成功。
- 新增或移动文档后检查相对链接（仓库内可用任意 Markdown 链接检查工具）。

## 6. 发布

打包、官方预构建镜像发布和版本发布流程见 [打包与发布](release.md)。
