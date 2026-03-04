# Confidential Agent

基于 Intel TDX 的机密计算 AI Agent 部署方案。Confidential Agent 将 AI Agent 的运行时环境与底层云基础设施进行硬件级隔离，通过远程证明机制向用户证明代码运行在真实的可信执行环境（TEE）中，同时确保敏感配置（如大模型 API Key、业务数据）在整个生命周期中都对云厂商不可见。本方案采用双节点架构——Trustee 负责远程证明和密钥管理，OpenClaw 负责运行 AI Agent 逻辑——两者协同工作，为企业和个人用户提供可验证的 AI 服务可信性。

## 概述

Confidential Agent 是一种运行在硬件级可信执行环境（TEE）中的 AI Agent 部署方案。它基于 Intel TDX（Trust Domain Extensions）技术，将 AI Agent 的执行环境与云基础设施完全隔离，确保即使云厂商也无法窥探或篡改 Agent 的运行状态和数据。这种架构特别适合对数据隐私和计算完整性有严格要求的场景，例如处理敏感业务数据的企业 AI 助手、需要向用户证明可信性的金融或医疗 AI 服务等。

Confidential Agent 由三个核心机密计算组件构成：Attestation Agent 提供 TDX 远程证明能力，在系统启动时收集和报告可信证据；Trustee 作为远程证明的验证方和密钥管理中心，负责验证运行环境的真实性并安全地分发敏感配置；TNG（Trusted Network Gateway）则提供加密通信能力，确保数据在传输过程中的安全性。在 Agent 启动时，Attestation Agent 首先向 Trustee 提供远程证明证据，Trustee 验证通过后才会通过 KBS（Key Broker Service）将磁盘密钥、API Key 等敏感数据安全注入到 Agent 环境中。这种设计使得敏感配置在整个生命周期中都受到保护——既不会以明文形式存储在镜像中，也不会在运行时被云厂商获取。

在运行时的通信架构上，Confidential Agent 采用分层的安全设计。部署在云端的是 OpenClaw Agent 节点和 Trustee 节点，两者位于同一 VPC 内并通过内网通信，Trustee 在完成远程证明验证后向 OpenClaw 下发必要的凭据和配置。用户通过本地运行的 TNG Client 与云端的 OpenClaw 建立连接，这一跳通信采用 rats-tls 协议——这是一种结合了双向 TLS 和 TEE 远程证明的加密协议，确保通信双方都是可信的 TEE 环境。用户的数据通过这条加密隧道传输到 OpenClaw Agent，由 Agent 调用外部大模型 API 完成推理后返回结果。整个过程中，用户的输入数据和 AI 的响应数据都经过端到端的加密保护，即使在公共网络上传输也不会被窃取或篡改。

## 威胁模型

将 AI Agent 部署在云端面临着多重隐私和安全威胁。首先是来自外部攻击者的威胁：恶意用户可能试图通过网络攻击窃取传输中的敏感数据，或者利用漏洞入侵实例获取商业机密。其次是来自云厂商自身的威胁：作为基础设施提供商，云厂商拥有物理机的完全控制权，理论上可以窥探内存中的数据、提取存储在磁盘上的配置、甚至篡改运行的代码。对于处理敏感业务数据或用户隐私信息的 AI Agent 来说，这些威胁都是不可接受的。

Confidential Agent 的设计目标是在不可信的云环境中建立一个可信的执行环境。为了实现这一目标，我们采用了纵深防御策略。首先，镜像构建完全在本地进行，不依赖任何云上构建服务，避免构建过程中被植入恶意代码。构建完成的镜像通过 dm-verity 进行完整性保护，任何对 rootfs 的篡改都会导致启动失败。镜像上传到云后，运行在 Intel TDX 提供的可信执行环境（TEE）中，这是一个硬件隔离的内存区域，即使云厂商拥有物理机的 root 权限也无法读取其中的数据。最后，在实例启动时，Trustee 会对运行环境进行远程证明验证，只有验证通过的环境才能获取解密密钥和敏感配置，确保即使镜像被复制到非 TEE 环境也无法正常运行。通过这一系列机制，Confidential Agent 确保用户的业务数据、API Key 等敏感信息在整个生命周期中都对攻击者和云厂商保持机密。

## 可信架构

### 系统分层与信任链

Confidential Agent 的信任链从底层硬件开始逐层向上传递，每一层的安全状态都会被度量并记录到 TDX Quote 和 EventLog 中，形成完整的可信证据链：

```
┌─────────────────────────────────────────┐
│  Layer 4: OpenClaw Agent                │
│  - AI 请求处理、模型 API 调用              │
│  - 用户对话、配置、运行时数据               │
├─────────────────────────────────────────┤
│  Layer 3: Guest OS (Alinux3)            │
│  - dm-verity 保护 rootfs 完整性           │
│  - dm-crypt 加密 data 卷（overlayfs）     │
├─────────────────────────────────────────┤
│  Layer 2: UKI (Unified Kernel Image)    │
│  - UEFI 引导程序 + 内核 + initrd          │
│  - initrd 中从 Trustee 获取磁盘密钥        │
│  - 解密 data 卷后切换 root                │
├─────────────────────────────────────────┤
│  Layer 1: Alibaba Cloud g8i (Intel TDX) │
│  - TDX 内存加密引擎 (MEE)                 │
│  - 硬件级信任根                           │
└─────────────────────────────────────────┘
        │
        │
        ▼
┌─────────────────────────┐
│    TDX Quote            │
│    + EventLog           │
└─────────────────────────┘
```

最底层是阿里云 g8i 实例提供的 Intel TDX 可信执行环境，TDX 通过内存加密引擎（MEE）对整个 Guest OS 的内存进行透明加密。系统采用 UKI（Unified Kernel Image）启动方式，将 UEFI 引导程序、内核、initrd 打包为单个 EFI 可执行文件，由 `10-install-attestation.sh` 配置生成。initrd 中的 `cai-secret-fetch` 服务（由 `13-install-secret-supplicant.sh` 安装）在启动早期通过 TNG 连接 Trustee KBS，获取磁盘加密密钥后解密 data 卷，然后切换到 rootfs 继续启动。

### 部署与运行时架构

```
  ┌───────────────────────────────────────────────────────────────────────┐   ┌─────────────┐
  │                              VPC                                      │   │     IM      │
  │                                                                       │   │  (钉钉/其他) │
  │   ┌─────────────────────┐                 ┌─────────────────────┐     │   └──────┬──────┘
  │   │      Trustee        │    远程证明      │     OpenClaw        │─────┼──────────┘
  │   │     阿里云 ECS       │────────────────→│    阿里云 TDX 机密    │     │
  │   │                     │   & 密钥分发     │      计算实例         │     │
  │   └──────[8081/tcp]─────┘                 └────[18789/tcp]──────┘     │
  │              ▲                             ▲         ▲                │
  └──────────────┼─────────────────────────────┼─────────┼────────────────┘
                 │                             │         │
                 │ 参考值注册            镜像上传 │         │
                 │                             │         │ RATS-TLS
                 │                             │         │ 远程证明
  ┌──────────────┼─────────────────────────────┼─────────┼────────────────┐
  │              │         本地环境             │         ▼                │
  │   ┌────────────────────────┐               │  ┌──────────────────┐   │
  │   │      镜像构建           │───────────────┘  │   TNG Client     │   │
  │   └────────────────────────┘                  └───[18789/tcp]────┘   │
  │                                                       │               │
  │                          ┌────────────────────────────┤               │
  │                          │             │              │               │
  │                       ┌──┴──┐      ┌───┴───┐      ┌───┴───┐           │
  │                       │浏览器│      │  TUI  │      │  手机  │  ...      │
  │                       └─────┘      └───────┘      └───────┘           │
  └───────────────────────────────────────────────────────────────────────┘

```

架构中的核心组件包括：Trustee 作为远程证明和密钥管理中心，通过端口 8081 提供 KBS 和 Attestation 服务，负责验证 TEE 环境的真实性并安全分发敏感配置；OpenClaw 是运行在 TDX 环境中的 AI Agent，通过 TNG Gateway（端口 18789）接收用户请求并对接外部模型服务；TNG（Trusted Network Gateway）实现 RATS-TLS 协议，在用户本地运行的 TNG Client 与云端 OpenClaw 之间建立加密通道，确保数据传输安全。

## 远程证明机制

远程证明是 Confidential Agent 安全架构的核心机制，它允许 Trustee 验证 OpenClaw 实例确实运行在真实的 Intel TDX 硬件环境中，只有在验证通过后才下发敏感配置。

### TDX Quote 生成

当 OpenClaw 实例启动时，initrd 中的 Attestation Agent 会收集 TDX 证据并生成 Quote（引证）。Quote 是一个密码学签名的数据结构，包含以下关键信息：

- **MrTd**: 整个可信域的内存度量值，对应构建时生成的 `cai-final-*.json` 中的 `measurement.uki.SHA-384` 字段
- **TcbVersion**: TDX 可信计算基版本号
- **EventLog**: 启动过程中的度量事件链

Quote 由 Intel TDX 模块使用平台私钥签名，确保其不可伪造。Attestation Agent 通过 TNG 将 Quote 发送给 Trustee 进行验证。

### 验证流程

Trustee 的 Attestation Service 收到 Quote 后执行多层次的验证：

1. **签名验证**: 调用 Intel PCS（Provisioning Certification Service）验证 Quote 的签名是否来自合法的 Intel TDX 平台
2. **参考值比对**: 从 RVPS（Reference Value Provider Service）获取预注册的参考值，比对 Quote 中的 MrTd 与参考值是否一致
3. **版本检查**: 检查 TcbVersion 是否符合安全策略要求

只有通过所有验证，Trustee 才会认定该 OpenClaw 实例是可信的，并授权 KBS 下发磁盘密钥和敏感配置。

### 参考值管理

参考值（Reference Value）是远程证明的"黄金标准"，它代表了可信系统镜像的预期状态。在镜像构建阶段，`image/build.sh` 在构建完成时调用 `cryptpilot-fde show-reference-value` 工具计算 UKI 的 SHA-384 哈希值，生成参考值文件。以 `cai-final-prod-202602261651.json` 为例：

```json
{
  "measurement.uki.SHA-384": [
    "aa1c6086ed05f3c9ebe767301914ea23aeff9aa1deb090845305e730ebb7573db7e9000b7d30bd3583c4a4e3a618570f"
  ]
}
```

部署时，Trustee 在初始化时从 OSS 读取参考值文件并注册到 RVPS。运行时，Trustee 比对 OpenClaw 上报的 MrTd 与预注册的参考值，确保实例没有被篡改或替换。

## 数据安全

Confidential Agent 在数据的整个生命周期中提供保护：内存中的数据通过 TDX 加密，落盘数据通过 dm-crypt 加密，传输中的数据通过 TNG 的 RATS-TLS 加密。

### 内存加密

Intel TDX 的内存加密引擎（MEE）对 Guest OS 的所有内存访问进行透明加密。这意味着即使攻击者物理接触服务器、使用冷启动攻击或内存嗅探工具，也无法读取 OpenClaw 运行时的内存内容。

OpenClaw 运行时的以下敏感数据都在加密内存中处理：
- 用户的对话 Prompt 和 AI 响应
- 百炼 API Key（从 KBS 获取后驻留内存，由 `13-install-secret-supplicant.sh` 配置获取）
- 钉钉 Bot 的 access token
- 对话上下文和临时计算结果

### 落盘保护

镜像由两个 LVM 卷组成，配置定义在 `image/disk-crypt/fde.toml`：

- **rootfs**: 只读，dm-verity 保护完整性，存放系统文件和 OpenClaw 二进制。任何对系统文件的篡改都会导致启动失败。
- **data**: 可写，dm-crypt 加密，存放 overlayfs 的增量数据（即 rootfs 上的所有写入操作实际落盘到 data 卷）。

OpenClaw 会写入 data 卷的数据包括：
- `/root/.openclaw/` 下的配置文件、状态和记忆信息

加密密钥（disk_passphrase）由 Trustee 通过 KBS 在远程证明通过后注入。initrd 中的 `cai-secret-fetch` 服务通过 TNG 连接 Trustee KBS，获取密钥后写入 `/run/cai/secrets/disk_key`，然后 `cryptpilot-fde-before-sysroot.service` 使用该密钥解密 data 卷。这意味着即使云厂商复制了磁盘镜像，没有 Trustee 的授权也无法解密数据。

### 传输加密（TNG RATS-TLS）

用户本地与云端 OpenClaw 之间的通信通过 TNG 实现 RATS-TLS 协议保护，加密以下内容：
- 用户发送的 HTTP 请求（包含对话内容）
- OpenClaw 返回的 AI 响应
- 可能包含的敏感上下文信息

具体实现：本地 TNG Client 与 OpenClaw 的 TNG Gateway（端口 18789）建立 RATS-TLS 连接。TNG 在标准 TLS 握手的基础上增加了 TEE 证明交换，确保通信双方都是可信的 TEE 环境后才建立加密通道。这防止了中间人攻击和伪造服务，确保用户数据只发送给经过验证的可信接收方。

## 目录结构

```
cai/
├── image/                     # 镜像构建（本地执行）
│   ├── build.sh               # 主构建脚本
│   ├── customize/             # 镜像定制资源
│   │   ├── script/            # 安装脚本
│   │   │   ├── 01-install-base.sh
│   │   │   ├── 10-install-attestation.sh
│   │   │   ├── 11-install-tng.sh
│   │   │   ├── 12-configure-ssh.sh
│   │   │   ├── 13-install-secret-supplicant.sh
│   │   │   ├── 50-install-openclaw.sh
│   │   │   ├── 51-install-openclaw-tdx-skill.sh
│   │   │   └── 99-cleanup.sh
│   │   └── files/             # 配置文件
│   │       └── skill.md       # OpenClaw TDX skill
│   └── disk-crypt/            # 磁盘加密配置
│       └── fde.toml           # cryptpilot FDE 配置
├── terraform/                 # IaC 配置
│   ├── main.tf                # 主配置（VPC, Trustee, OpenClaw）
│   ├── variables.tf           # 变量定义
│   ├── outputs.tf             # 输出定义
│   ├── terraform.tfvars.example # 变量示例
│   └── modules/
│       ├── trustee/           # Trustee 证明服务模块
│       └── openclaw/          # OpenClaw Agent 模块
├── secrets/                   # 本地生成的密钥（gitignore）
└── Makefile                   # 构建和部署入口
```

## 快速开始

### 1. 准备工作

#### 必需工具

```bash
make install-deps
```

或手动安装：

```bash
yum install -y qemu-img wget terraform jq
```

#### 阿里云资源

1. **阿里云 RAM 账号** 及 AccessKey/SecretKey（[创建参考](https://help.aliyun.com/zh/ram/user-guide/create-an-accesskey-pair#section-rjh-18m-7kp)）

### 2. 生成 Secrets 和配置

```bash
make generate-secrets
```

此命令会：
- 生成磁盘加密密钥
- 生成 SSH 服务器密钥对
- 生成 OpenClaw 配置文件（需要手动编辑填入 API Key）

编辑 `secrets/openclaw.json`，填入：
- `<DASHSCOPE_API_KEY>` - 百炼 API Key（[获取参考](https://www.alibabacloud.com/help/zh/model-studio/openclaw)）
- `<DINGTALK_BOT_CLIENT_ID>` 和 `<DINGTALK_BOT_CLIENT_SECRET>` - 钉钉 Bot 凭证（[获取参考](https://help.aliyun.com/zh/simple-application-server/use-cases/quickly-deploy-and-use-openclaw)）

### 3. 构建机密镜像

```bash
make build-image
```

构建输出（在 `image/output/` 目录）：

**生产环境**：
- `cai-final-prod-{timestamp}.qcow2` - 生产镜像（禁止 SSH 登录）
- `cai-final-prod-{timestamp}.json` - 参考值文件

**调试环境**：
- `cai-final-debug-{timestamp}.qcow2` - 调试镜像（允许 SSH 密钥登录）
- `cai-final-debug-{timestamp}.json` - 参考值文件

### 4. 部署基础设施

复制配置模板：

```bash
cp terraform/terraform.tfvars.example terraform/terraform.tfvars
```

> **提示**：一般情况下使用默认配置即可，无需修改配置文件内容。如需限制服务访问 IP，编辑 `terraform.tfvars` 修改 `security_group_allowed_cidr`，默认为 `0.0.0.0/0`（允许所有 IP）

部署：

```bash
make deploy-infra
```

此命令会创建：
- VPC 和 VSwitch
- OSS Bucket 存储镜像
- Trustee ECS 实例（固定私网 IP）
- OpenClaw ECS 实例（固定私网 IP）

### 5. 建立安全连接

```bash
make connect-tng
```

此命令会：
- 在本地启动 TNG Client
- 建立到 OpenClaw 的 rats-tls 加密隧道
- 将 OpenClaw 服务映射到本地端口

### 6. 访问 OpenClaw 服务

连接成功后将看到类似如下信息：

```
🚀 Launching TNG Client Container...
   Access Information:
      OpenClaw Control UI URL:   http://localhost:18789/openclaw
      OpenClaw Gateway URL:      ws://localhost:18789/ (for TUI/App remote access)
      OpenClaw Gateway Token:    19cbddecb183cd5c728c762413ffd17bd8873f6c
```

用户可以通过以下方式访问 OpenClaw：

- **浏览器**：打开 `http://localhost:18789/openclaw` 访问 OpenClaw 控制界面
- **TUI / 本地 APP**：通过 WebSocket 地址 `ws://localhost:18789/` 以 remote 方式连接，使用输出中提供的 Gateway Token 进行认证
- **钉钉 Bot**：在钉钉群中 @Bot 发送消息，Bot 将通过 OpenClaw 处理请求并回复（需在 `secrets/openclaw.json` 中配置钉钉凭证）

### 7. 销毁部署资源

如需清理所有部署的云上资源，执行：

```bash
make destroy-infra
```

此命令会销毁：
- Trustee ECS 实例
- OpenClaw ECS 实例
- VPC 和 VSwitch
- OSS Bucket（及其中的镜像文件）
- 安全组和其他相关网络资源

> **警告**：此操作不可逆，执行前请确认已备份必要数据。

## 故障排除

### Terraform 部署前必须操作

#### 1. 授权 ECS 访问 OSS 以导入镜像

**问题**: 首次导入自定义镜像时，ECS 服务账号需要权限访问 OSS bucket

**解决方案**:

**方法 A - 控制台一键授权（推荐）**:
1. 登录阿里云控制台
2. 进入 **ECS** → **镜像** → **导入镜像**
3. 点击 **"授权"** 按钮
4. 按提示完成授权

**方法 B - RAM 控制台手动创建角色**:
1. 进入 **RAM 访问控制** → **角色管理** → **创建角色**
2. 选择 **"阿里云服务"** → **"云服务器 ECS"**
3. 角色名称: `AliyunECSImageImportDefaultRole`
4. 添加权限: `AliyunOSSFullAccess`

### Terraform 部署错误排查

| 错误 | 原因 | 解决方案 |
|------|------|----------|
| `ErrorCode=UserDisable` | OSS 服务未开通 | 登录控制台 → 对象存储 OSS → 开通服务 |
| `ErrorCode=AccessDenied` | RAM 用户缺少 OSS 权限 | 给 RAM 用户添加 `AliyunOSSFullAccess` |
| `InvalidAccessKeyId.NotFound` | AccessKey 无效 | 检查 `ALICLOUD_ACCESS_KEY` 环境变量 |
| `NoSetRoletoECSServiceAcount` | ECS 服务账号无 OSS 权限 | 参见上方"授权 ECS 访问 OSS" |

### 镜像构建问题

| 问题 | 原因 | 解决方案 |
|------|------|----------|
| 下载基础镜像失败 | 网络问题 | 手动下载并放入 `image/output/.base-image.qcow2` |

## 注意事项

1. **实例规格**: 必须使用支持 Intel TDX 的 g8i 实例规格
2. **地域限制**: TDX 实例仅在部分地域可用（如北京、上海）
3. **镜像复用**: `cai-intermediate-full.qcow2` 可重复使用，加速后续构建
4. **Secret 安全**: `secrets/` 目录包含敏感密钥，已加入 `.gitignore`，请勿提交到 Git

## License

Confidential Agent is licensed under the Apache License 2.0. See [LICENSE](LICENSE) for the full license text.
