# confidential-agent RPM 安装与 all-in-one 流程

当前支持 Alibaba Cloud Linux 3、x86_64。confidential-agent 使用原生工具，标准
all-in-one 构建和部署不需要宿主机 Docker 或 Podman。Guest 内应用如果启用
PEP，仍可以使用 Guest 自己的容器隔离。

## 构建 RPM

在打包机安装构建依赖，然后从源码根目录执行：

```bash
sudo dnf install -y rpm-build cargo rust gcc glibc-devel pkgconf-pkg-config python3.11 curl tar
packaging/build-rpm.sh
```

产物位于 `target/rpm/RPMS/x86_64/cagent-0.1.0-1.al8.x86_64.rpm`，
SRPM 位于 `target/rpm/SRPMS/`。源码准备阶段缓存并校验固定版本的
cosign、aliyun、ossutil，GitHub 下载默认先尝试国内代理再回退官方地址；
可用 `CA_GITHUB_PROXY_URL` 修改代理。`target/rpm/SOURCES/SHA256SUMS`
记录源码、Cargo vendor 和工具归档的哈希。rpmbuild 内的 Cargo 构建和测试
使用 `--locked --offline`，不会再次下载依赖。

打包输出目录同时包含配套 Shelter RPM、`shelter-rpm.json` 和两包的
`SHA256SUMS`。Shelter 当前来自提交 `36a792a913ba3bf86612e3999ee262de7ec4448c`，
在主线运行时和 VMM 基础上保留可移植镜像发布、可选 manifest 版本固定、
ext4 稀疏文件提取与副本生命周期功能。CA 使用显式 v1 构建配置和 v2 运行时配置。
内置 Cryptpilot guest 使用官方 0.9.3，CA 安装预检要求 host 和 guest 均为 0.9.3。
Cryptpilot 启动配置直接使用最终根设备 `/dev/mapper/rootfs`，避免标签扫描探测内部增量设备。
Shelter RPM 显式依赖 `zstd`，安装时由 DNF 自动解析，以支持默认的 Zstd initrd 压缩。
`hack/shelter-rpm.json` 记录源码提交、RPM SHA-256 和 payload 摘要；
准备 cagent 源码包时会核对该记录与实际 Shelter RPM。

`CAGENT_BUILD_JOBS` 控制打包并行度，默认 4。最终 release 固定为 `1`，
本地验证过程不递增 RPM 的 Release。

## 在干净机器安装

把 confidential-agent 和随项目发布的 Shelter RPM 放在同一目录，安装：

```bash
sha256sum -c SHA256SUMS
sudo dnf install -y ./shelter-0.3.2-1.al8.x86_64.rpm ./cagent-0.1.0-1.al8.x86_64.rpm
confidential-agent version
rpm -q cagent shelter attestation-challenge-client libtdx-verify trusted-network-gateway
```

RPM 包名为 `cagent`，正式命令为 `confidential-agent`；`cagent` 是兼容别名。
如果已安装相同版本号的旧包，需对相应本地 RPM 执行 `dnf reinstall -y`，
普通 `dnf install` 不会替换同版本旧内容。可用 `rpm -q --qf '%{PAYLOADDIGEST}\n' shelter`
与 `shelter-rpm.json` 中的 `payload_sha256` 比较，确认实际安装的 Shelter 内容。
RPM 安装 CLI、Guest 构建资产和示例，并自动解析原生验证工具的依赖。
不需要在这台机器安装 Rust/Cargo、克隆源码或编译 CA。
分阶段模板为只读基础镜像之外的加密可写层预留磁盘空间；Hermes 默认使用 60 GB 部署盘。
发布使用工作区持久化的独立 OSS 命名，已有发布记录继续保留其原 bucket 和对象位置。
目前 Shelter RPM 仍携带构建依赖，因此这也是可用于构建镜像的安装环境。

初始化向导可直接读取 RPM 内的模板，无需源码目录：

```bash
export PATH=/usr/libexec/cagent/bin:$PATH
aliyun configure
confidential-agent init
```

先选择构建或部署，再选择模板。也可以显式生成共享镜像构建目录：

```bash
confidential-agent init allinone --stage build --non-interactive --output-dir ~/cagent-work
cd ~/cagent-work/allinone-builder
```

生成目录包含 `build.yaml`、完整构建脚本及 `NEXT_STEPS.md`。构建阶段不要求模型 API key。
`all-in-one` 是 `allinone` 的别名。OpenClaw、Hermes、Codex、Claude Code、
OpenClaw-vLLM 和 vLLM inference 同样支持 `--stage build/deploy`。
旧的显式目标命令（例如不带 `--stage` 的 `init hermes`）仍保留原来的合并模板行为。

CA 的发布和云部署会读取宿主机的 AK/StsToken profile；也可使用
AccessKey 环境变量。凭证优先级和命名 profile 见
[all-in-one 文档](../examples/allinone/README.md#build-and-publish)。

## 构建和发布

进入 init 生成的 `allinone-builder` 目录，检查其中的 `build.yaml`，然后执行：

```bash
confidential-agent build --spec build.yaml
confidential-agent image publish allinone-builder --spec build.yaml
test -f publisher.key || (umask 077; openssl genpkey -algorithm ED25519 -out publisher.key)
openssl pkey -in publisher.key -pubout -out publisher.pem
confidential-agent image export allinone-builder \
  --signing-key publisher.key --output image-release.json
```

manifest 摘要会从经过验签的 release 自动读取，无需抄写。镜像上传的
`CA_IMAGE_OSS_ENDPOINT=auto` 会检测同地域 ECS 内网端点，否则使用公网。
发布不重新构建镜像。

## 部署和对话

使用已发布的 release 为每个独立实例生成部署目录。通过交互向导填写信任信息、
harness、运行模式、模型和凭证，或使用参数：

交互提示的方括号表示按回车即可采用的默认值。模型名与非交互模式使用同一份
`software-versions.json` 默认配置；传入 `--model` 或设置 `DASHSCOPE_MODEL` 时直接使用指定值，
不再询问。地域、可用区、实例规格、磁盘大小、运行模式和输出目录也提供默认值。

```bash
# 在模型 API key 已通过 DASHSCOPE_API_KEY 设置的环境中执行。
confidential-agent init allinone --stage deploy --non-interactive \
  --service-id hermes-01 --harness hermes --harness-mode gateway \
  --image-release ./image-release.json --trusted-key ./publisher.pem \
  --reference-values sample --output-dir ~/cagent-work
cd ~/cagent-work/hermes-01
```

部署目录只包含 `instance.yaml`、release、公钥、私有配置及后续步骤，不包含构建输入。
签名和镜像兼容性会在 deploy 创建云资源前验证。
只有需要额外固定某个版本时才传 `--manifest sha256:<完整摘要>`；省略时仍会验签，并把解析出的摘要固定在部署状态中。
再次 init 时换 `--service-id`，可复用相同镜像创建同种或不同 harness 的实例。
allinone 支持 OpenClaw、Hermes、Codex、Claude Code、QoderCLI、OpenCode；
Qoder 使用自己的账户登录流程，不把 DashScope key 当成 Qoder 凭证。
模型、凭证使用生成的私有资源，不写入共享镜像。

`--replicas N` 表示一个服务的副本，共享服务配置与身份；gateway/app-server 默认使用 NLB，
vLLM inference 保持 ALB + OHTTP。CLI 模式应使用不同的 service ID 创建独立实例。副本不会自动同步应用文件或会话历史。
同时运行的不同 harness 使用不同服务 ID。同一 all-in-one 实例在 CLI harness 之间切换时，
可以保持 service ID、release 和基础设施设置不变，使用新的输出目录生成部署配置，
再运行 `confidential-agent inject --spec 新配置/instance.yaml --target-ip 实例IP`；实例及其数据保留。
切换监听端口或负载均衡拓扑时，创建新的部署。多条本地连接使用不同 `--ready-json`，实际本地端口读取
ready 文件的 `client_endpoints`，避免假设所有连接都能占用相同端口。

```bash
confidential-agent peering add --role operator --cidr DEPLOYER_PUBLIC_IP/32 --label deployer
confidential-agent spec validate --spec instance.yaml
confidential-agent deploy --spec instance.yaml
confidential-agent status --live
confidential-agent report --json
confidential-agent ssh SERVICE -- cai-agent --check
confidential-agent ssh SERVICE --tty -- cai-agent
```

最后一条命令进入所选 harness 的交互入口。配置模型后发送实际问题并确认
模型返回内容；`active` 或端口就绪只说明部署状态，不等同于模型对话成功。
Gateway 的 HTTP/WebSocket 客户端使用原生 TNG 隧道：

```bash
confidential-agent connect start --service SERVICE --ready-json connect.json --log-file connect.log
confidential-agent connect stop --ready-json connect.json
confidential-agent destroy SERVICE
```

后台连接记录使用 v2 格式，包含 PID、进程启动时刻、系统 boot ID 和日志
路径，停止时校验进程身份。旧 tools 容器产生的 v1 ready 文件不用于管理
原生隧道，升级前应先停止旧连接。

销毁消费者不会删除共享云镜像。发布者确认无消费者依赖后，使用
`image unpublish` 退役镜像；导出过的镜像需要显式 `--force`。

`status --live` 汇总副本就绪状态；`report --json` 的 `replicas` 数组包含各物理实例的证明和
管理状态，查询直接访问已记录的实例地址。负载均衡地址仅用于应用流量。只要有副本收集失败，
报告不会把整个服务视为证明收集成功；兼容字段中的 EAR 是首个实例的证明，逐副本检查使用 `replicas`。

## 目录和已有镜像

| 位置 | 用途 |
|---|---|
| `./confidential-agent-init/allinone-builder/` | 从执行 init 时的目录生成；包含 `build.yaml`、构建脚本、NEXT_STEPS。可由 `--output-dir` 改变 |
| `./confidential-agent-init/<service-id>/` | 独立消费者的 `instance.yaml`、私有资源、release 与可选公钥 |
| `~/.confidential-agent/` | 默认 CA state，按操作用户的 HOME 定位；与当前工作目录无关 |
| `<state>/services/<service-id>/` | 构建/发布记录、构建清单、实例身份、密钥、运行状态 |
| `<state>/services/<service-id>/shelter/` | CA 管理的 Shelter 构建记录与 Terraform 工作目录，不要另建同名独立 Shelter 工作区替代 |
| `<state>/services/<service-id>/shelter/cache/` | Shelter 的基础镜像等构建缓存；包解析缓存另在 `shelter/inputs/resolver-cache/` |
| 源码中的 `one-click/` | 另一套安装/操作脚本入口，RPM init 流程不需要进入它 |
| `<state>/one-click/` | one-click 默认生成工作目录；`--work-dir` 可单独指定 |
| `/usr/share/cagent/` | RPM 提供的模板资产，由 init 复制到可编辑项目目录 |

NEXT_STEPS 会写入明确的 `cd` 和绝对 `--state-dir`。自定义 state 时所有操作使用同一个绝对路径；
不同用户或遗漏该参数会看到另一套状态。相对 `--spec`、签名密钥和 `--output` 路径按命令执行目录解释；
spec 内的文件路径按 spec 所在目录解释。导出的 `image-release.json` 放在指定输出位置，不会代替 state。
不要把清空 state 当作清理缓存，也不要通过重新 init --force 来修复 export。
旧 CA 清单的 `cache_dir` 可能指向未使用的 `<service>/cache`；新记录修正为实际 Shelter cache 路径，升级不会迁移或删除旧目录。
`shelter/artifacts`、`records`、`proofs`、`images` 和实例密钥是构建/证明或运行记录，不应作为普通缓存清空。

已构建且发布的镜像在更新 CLI/Shelter 后可以直接重新 export，无须重建或重发。
新 release 不包含 UID；旧 release/spec/target 中的 `account_id` 作为兼容字段读取并忽略。
共享镜像或云市场镜像已有可用 ID 时，发布者使用原构建记录直接签发映射：

```bash
confidential-agent image export allinone-builder --region cn-beijing --image-id m-EXISTING \
  --signing-key publisher.key --output image-release.json
```

消费者仍需发布者提供的 release（含参考值和运行兼容信息），仅有云镜像 ID 不能推导可信参考值。
消费者可在 `deploy.image.image_id`、`init --image-id` 或 `deploy --image-id` 指定自己地域中的已有镜像 ID。
该选择不修改 release 中的参考值，不上传、导入或取得共享镜像的生命周期所有权。
镜像的云端共享权限、云市场订阅由云平台管理，CA 不替用户购买或订阅。

## 显式跳过发布签名

默认要求 `trusted_key` 并验签。可以显式执行：

```bash
confidential-agent deploy --spec instance.yaml --skip-signature-verification
```

也可在 init 时传同名参数，生成 `deploy.image.skip_signature_verification: true`；此时无需公钥。
仅跳过 release 的发布签名，不跳过 manifest 固定摘要、参考值结构/关联检查、runtime 兼容检查或运行时证明。
跳过后文件中的参考值没有发布签名的来源保证，使用者需要自行信任该来源。
CLI 覆盖选项会记录到部署快照；使用原 spec 恢复同一部署时应传相同覆盖选项，或使用包含该设置的 spec。
原有镜像与构建记录保持可用；用户现有 NEXT_STEPS 是普通项目文件，不会随 RPM 升级自动重写。
