.PHONY: build-provider deploy-infra clean help all destroy-infra clean-provider clean-image clean-all generate-secrets deploy-trustee deploy-openclaw destroy-trustee destroy-openclaw init-terraform build-image dev-trustee dev-openclaw dev-tng connect-tng clean-dev-trustee clean-dev-openclaw clean-dev-tng clean-dev-all show-info

# OpenClaw configuration template (<GATEWAY_TOKEN> will be replaced during generation)
define OPENCLAW_CONFIG_TEMPLATE
{
  "models": {
    "mode": "merge",
    "providers": {
      "bailian": {
        "baseUrl": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "apiKey": "<DASHSCOPE_API_KEY>",
        "api": "openai-completions",
        "models": [
          {
            "id": "qwen3-max-2026-01-23",
            "name": "qwen3-max-2026-01-23",
            "reasoning": false,
            "input": ["text"],
            "contextWindow": 262144,
            "maxTokens": 65536
          },
          {
            "id": "qwen3-coder-plus",
            "name": "qwen3-coder-plus",
            "reasoning": false,
            "input": ["text"],
            "contextWindow": 131072,
            "maxTokens": 32768
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "bailian/qwen3-max-2026-01-23"
      }
    }
  },
  "plugins": {
    "enabled": true,
    "allow": ["dingtalk"]
  },
  "channels": {
    "dingtalk": {
      "enabled": true,
      "clientId": "<DINGTALK_BOT_CLIENT_ID>",
      "clientSecret": "<DINGTALK_BOT_CLIENT_SECRET>",
      "dmPolicy": "open",
      "allowFrom": ["*"],
      "groupPolicy": "open",
      "debug": false,
      "messageType": "markdown"
    }
  },
  "gateway": {
    "mode": "local",
    "bind": "lan", // It is safe to expose this gateway to 0.0.0.0, since it is behind the trusted-network-gateway.
    "port": 18789,
    "auth": {
      "mode": "token",
      "token": "<GATEWAY_TOKEN>"
    },
    "controlUi": {
      "enabled": true,
      "basePath": "/openclaw",
      "dangerouslyAllowHostHeaderOriginFallback": true, // Required when bind=lan
      "dangerouslyDisableDeviceAuth": true // Disable device auth since we do not always provide SSH access
    }
  }
}
endef
export OPENCLAW_CONFIG_TEMPLATE

# TNG client configuration template (<AS_URL> and <OPENCLAW_ADDR> will be replaced during generation)
define TNG_CLIENT_CONFIG_TEMPLATE
{
    "add_ingress": [
        {
            "mapping": {
                "in": {
                    "host": "0.0.0.0",
                    "port": 18789
                },
                "out": {
					"host": "<OPENCLAW_ADDR>",
					"port": 18789
                }
            },
            "verify": {
                "as_addr": "<AS_URL>",
                "policy_ids": [
                    "default"
                ]
            }
        }
    ]
}
endef
export TNG_CLIENT_CONFIG_TEMPLATE

# Install system dependencies
install-deps:
	@echo "📦 Installing system dependencies..."
	@yum install -y qemu-img wget jq unzip docker
	@if systemctl list-unit-files | grep -q docker.service; then \
		systemctl enable docker --now; \
	fi
	@yum install -y go && go env -w GO111MODULE=on && go env -w GOPROXY=https://goproxy.cn,direct
	@cd /tmp/ && wget https://releases.hashicorp.com/terraform/1.14.6/terraform_1.14.6_linux_amd64.zip
	@unzip -o /tmp/terraform_1.14.6_linux_amd64.zip -d /usr/local/bin
	@rm -f /tmp/terraform_1.14.6_linux_amd64.zip
	@yum install -y cryptpilot-fde
	@echo "✅ System dependencies installed"

# Default target - show help
help:
	@echo "🚀 CAI Deployment Makefile"
	@echo "========================"
	@echo ""
	@echo "ℹ️  Help:"
	@echo "  ℹ️  help           - Show this help message"
	@echo ""
	@echo "📋 Core Workflows:"
	@echo "  🔄 all             - Complete end-to-end deployment workflow (build→deploy)"
	@echo ""
	@echo "🔧 Setup:"
	@echo "  📦 install-deps   - Install system dependencies (qemu-img, wget, terraform, jq)"
	@echo ""
	@echo "🔧 Preparation:"
	@echo "  🔐 generate-secrets - Generate required secret files"
	@echo ""
	@echo "🏗️  Build Targets (build-*):"
	@echo "  🛠️  build-image     - Build VM images"
	@echo ""
	@echo "☁️  Deploy Targets (deploy-*):"
	@echo "  ☁️  deploy-infra    - Deploy complete infrastructure"
	@echo "  ☁️  deploy-trustee  - Deploy Trustee service only (for testing)"
	@echo "  ☁️  deploy-openclaw - Deploy OpenClaw service"
	@echo ""
	@echo "🔐 Production Connection:"
	@echo "  🔐 connect-tng     - Connect TNG Client to production environment (uses public IP, port 18789)"
	@echo ""
	@echo "🔧 Local Development (dev-*):"
	@echo "  🔧 dev-trustee     - Start local Trustee development container"
	@echo "  🔧 dev-openclaw    - Start local OpenClaw QEMU development environment"
	@echo "  🔧 dev-tng         - Start local TNG Client (connects to local services)"
	@echo ""
	@echo "🧹 Local Cleanup (clean-dev-*):"
	@echo "  🧹 clean-dev-trustee   - Stop and remove local Trustee development container"
	@echo "  🧹 clean-dev-openclaw  - Stop and remove local OpenClaw development container"
	@echo "  🧹 clean-dev-tng       - Stop and remove local TNG Client container"
	@echo "  🧹 clean-dev-all       - Stop and remove all local development containers"
	@echo ""
	@echo "🧹 Clean Targets (clean-*):"
	@echo "  🧹 clean-image     - Clean image build artifacts"
	@echo "  🧹 clean-all       - Clean all build artifacts"
	@echo ""
	@echo "💥 Destroy Targets (destroy-*):"
	@echo "  💥 destroy-infra   - Destroy deployed infrastructure"
	@echo "  💥 destroy-trustee - Destroy deployed Trustee service"
	@echo "  💥 destroy-openclaw - Destroy deployed OpenClaw service"
	@echo ""
	@echo "📊 Information:"
	@echo "  📊 show-info      - Show deployment information (IPs, URLs, SSH commands)"


# Build the custom terraform provider
build-provider:
	@if [ -f "terraform-provider-alicloud/bin/terraform-provider-alicloud" ]; then \
		echo "✅ Provider binary already exists, skipping build..."; \
	else \
		echo "🏗️  Building provider for Linux AMD64..."; \
		if [ ! -d "terraform-provider-alicloud" ]; then \
			echo "📥 Cloning terraform-provider-alicloud repository..."; \
			git clone --depth 1 -b feature/nvme-support https://gh-proxy.org/https://github.com/inclavare-containers/terraform-provider-alicloud.git; \
		else \
			echo "✅ terraform-provider-alicloud directory already exists, skipping clone..."; \
		fi && \
		cd terraform-provider-alicloud && \
		GOOS=linux GOARCH=amd64 go build -o bin/terraform-provider-alicloud . && \
		echo "" && \
		echo "✅ Build completed successfully!" && \
		echo "🔧 To use the custom provider with terraform manually, please run:" && \
		echo "  export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc" && \
		echo "" && \
		echo "🎯 Then you can use terraform commands as usual."; \
	fi


# Build VM images using build.sh
build-image: generate-secrets
	@echo "🛠️  Building CAI images..."
	@cd image && ./build.sh
	@echo "✅ Image build completed!"

# Initialize Terraform environment
init-terraform: build-provider
	@if [ ! -f terraform/terraform.tfvars ]; then \
		echo "📄 terraform.tfvars not found, creating from example..."; \
		cp terraform/terraform.tfvars.example terraform/terraform.tfvars; \
		echo "✅ Created terraform/terraform.tfvars from example"; \
		echo "⚠️  You may need to review and update the configuration before proceeding"; \
	fi
	@echo "📦 Initializing Terraform environment..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	echo "🔄 Running terraform init..."; \
	terraform init; \
	echo "✅ Terraform environment initialized";

# Deploy Trustee service for testing (depends on generate-secrets and init-terraform)
deploy-trustee: generate-secrets init-terraform
	@echo "🚀 Deploying Trustee service only..."
	@echo "🔧 Setting up Terraform environment..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	echo "⚡ Applying changes..." && \
	terraform apply -target=module.trustee -auto-approve
	@echo "🎉 Trustee deployment completed!"

# Deploy OpenClaw service (depends on generate-secrets and init-terraform)
deploy-openclaw: generate-secrets init-terraform
	@echo "☁️  Deploying OpenClaw service..."
	@echo "🔧 Setting up Terraform environment..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	echo "⚡ Applying changes..." && \
	terraform apply -target=module.openclaw -auto-approve
	@echo "🎉 OpenClaw deployment completed!"

# Deploy infrastructure using Terraform
deploy-infra: init-terraform
	@echo "🚀 Deploying CAI infrastructure..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform apply -auto-approve
	@echo "✅ Infrastructure deployment completed!"

# Complete end-to-end workflow
all: install-deps build-provider build-image generate-secrets init-terraform deploy-infra
	@echo ""
	@echo "===========================================" 
	@echo "✅ Complete deployment workflow finished!"
	@echo "===========================================" 
	@echo "🚀 Available operations:"
	@echo "  🔍 Check status: make status"
	@echo "  🧪 Test services: make test-services"
	@echo "  💥 Destroy environment: make destroy-infra"
	@echo "  🧹 Clean builds: make clean-all"

# Destroy deployed infrastructure
destroy-infra: init-terraform
	@echo "💥 Destroying infrastructure..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform destroy -auto-approve
	@echo "✅ Infrastructure destroyed"

# Destroy deployed Trustee service
destroy-trustee: init-terraform
	@echo "💥 Destroying Trustee service..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform destroy -target=module.trustee -auto-approve
	@echo "✅ Trustee service destroyed"

# Destroy deployed OpenClaw service
destroy-openclaw: init-terraform
	@echo "💥 Destroying OpenClaw service..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform destroy -target=module.openclaw -auto-approve
	@echo "✅ OpenClaw service destroyed"

# Clean provider build artifacts
clean-provider:
	@if [ -d "terraform-provider-alicloud" ]; then \
		echo "🧹 Cleaning terraform-provider-alicloud directory..."; \
		rm -rf terraform-provider-alicloud; \
	fi
	@echo "✅ Provider build artifacts cleaned"

# Clean image build artifacts
clean-image:
	@if [ -d "image/output" ]; then \
		echo "🧹 Cleaning image output directory..."; \
		rm -rf image/output/*; \
	fi
	@echo "✅ Image build artifacts cleaned"

# Clean secrets directory
clean-secrets:
	@if [ -d "secrets" ]; then \
		echo "🧹 Cleaning secrets directory..."; \
		rm -rf secrets; \
	fi
	@echo "✅ Secrets cleaned"

# Clean all build artifacts
clean-all: clean-provider clean-image clean-secrets
	@echo "🧽 All build artifacts cleaned successfully"

# Generate all required secrets locally
generate-secrets:
	@echo "🔑 Checking for existing secrets..."
	@mkdir -p secrets/
	
	@if [ ! -f "secrets/disk_passphrase" ]; then \
		echo "   🔐 Generating disk encryption passphrase..."; \
		openssl rand -hex 32 > secrets/disk_passphrase; \
	else \
		echo "   ✅ Using existing disk passphrase"; \
	fi
	
	@if [ ! -f "secrets/sshd_server_key" ]; then \
		echo "   🔐 Generating SSH server key pair..."; \
		ssh-keygen -t rsa -b 4096 -f secrets/sshd_server_key -N "" -C "openclaw-sshd-server"; \
	else \
		echo "   ✅ Using existing SSH server key"; \
	fi
	
	@if [ ! -f "secrets/ssh_client_key" ]; then \
		echo "   🔐 Generating SSH client key pair..."; \
		ssh-keygen -t rsa -b 4096 -f secrets/ssh_client_key -N "" -C "openclaw-ssh-client"; \
	else \
		echo "   ✅ Using existing SSH client key"; \
	fi
	
	@if [ ! -f "secrets/openclaw.json" ]; then \
		echo "   🔐 Generating OpenClaw configuration..."; \
		TOKEN=$$(openssl rand -hex 32 | cut -c1-40); \
		echo "$${OPENCLAW_CONFIG_TEMPLATE}" | sed "s/<GATEWAY_TOKEN>/$$TOKEN/g" > secrets/openclaw.json; \
		echo "⚠️  Please edit secrets/openclaw.json to configure your API keys"; \
	else \
		echo "   ✅ Using existing OpenClaw configuration"; \
	fi

	@echo "✅ Secret generation completed:"
	@echo "   📁 Disk passphrase: secrets/disk_passphrase"
	@echo "   📁 SSHD server key: secrets/sshd_server_key"
	@echo "   📁 SSH client key: secrets/ssh_client_key"
	@echo "   📁 OpenClaw config: secrets/openclaw.json"

# Complete cleanup (destroy infrastructure + clean builds)
clean: destroy-infra clean-all
	@echo "🧼 Environment completely cleaned"


# Show deployment information
show-info: init-terraform
	@echo "📊 Retrieving deployment information..."
	@echo ""
	@SSH_KEY_PATH="`realpath $$(pwd)/secrets/ssh_client_key`" && \
	export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	TRUSTEE_PUBLIC_URL=$$(terraform output -raw trustee_public_url 2>/dev/null || echo "N/A") && \
	TRUSTEE_PRIVATE_URL=$$(terraform output -raw trustee_private_url 2>/dev/null || echo "N/A") && \
	TRUSTEE_PUBLIC_IP=$$(terraform output -raw trustee_public_ip 2>/dev/null || echo "N/A") && \
	TRUSTEE_PRIVATE_IP=$$(terraform output -raw trustee_private_ip 2>/dev/null || echo "N/A") && \
	OPENCLAW_PUBLIC_IP=$$(terraform output -json openclaw_public_ip 2>/dev/null || echo "N/A") && \
	OPENCLAW_PRIVATE_IP=$$(terraform output -json openclaw_private_ip 2>/dev/null || echo "N/A") && \
	echo "=== 🔐 Trustee Service ===" && \
	echo "   Public IP:   $$TRUSTEE_PUBLIC_IP" && \
	echo "   Private IP:  $$TRUSTEE_PRIVATE_IP" && \
	echo "   Public URL:  $$TRUSTEE_PUBLIC_URL" && \
	echo "   Private URL: $$TRUSTEE_PRIVATE_URL" && \
	echo "" && \
	echo "=== 🤖 OpenClaw Service ===" && \
	echo "   Public IP:  $$OPENCLAW_PUBLIC_IP" && \
	echo "   Private IP: $$OPENCLAW_PRIVATE_IP" && \
	echo "" && \
	echo "=== 🔑 Remote Access ===" && \
	echo "SSH to OpenClaw:" && \
	echo "   ssh -i $$SSH_KEY_PATH root@$$OPENCLAW_PUBLIC_IP"


# Test image locally with QEMU
# Usage: make dev-openclaw [IMAGE_PATH=image/output/cai-final-debug-*.qcow2] [KVM=Y]
dev-openclaw: init-terraform
	@SSH_KEY_PATH="`realpath $$(pwd)/secrets/ssh_client_key`" && \
	export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	VSWITCH_CIDR=$$(cd terraform && terraform console 2>/dev/null <<< "var.vswitch_cidr" | tr -d '"') && \
	OPENCLAW_IP=$$(cd terraform && terraform console 2>/dev/null <<< "var.openclaw_private_ip" | tr -d '"') && \
	echo "🖥️  Starting OpenClaw QEMU test environment..." && \
	echo "   Image: $(IMAGE_PATH)" && \
	echo "   KVM: $(KVM)" && \
	echo "   Network: cai-test-net" && \
	echo "   OpenClaw IP: $$OPENCLAW_IP" && \
	echo "" && \
	echo "🔐 SSH Login:" && \
	echo "   ssh -i $$SSH_KEY_PATH root@$$OPENCLAW_IP" && \
	echo "" && \
	echo "   Press Ctrl+A X to stop" && \
	echo "" && \
	docker network inspect cai-test-net >/dev/null 2>&1 || \
		(echo "📡 Creating Docker network cai-test-net..." && \
		docker network create --subnet=$$VSWITCH_CIDR cai-test-net) && \
	docker rm -f -t 0 cai-test-openclaw 2>/dev/null || true && \
	docker run --rm -it --privileged \
		--name cai-test-openclaw \
		--network cai-test-net \
		--ip $$OPENCLAW_IP \
		-v "$(PWD):$(PWD):ro" \
		-e BOOT="" \
		-e KVM=$(KVM) \
		-e CPU_CORES=$$(nproc) \
		-e RAM_SIZE=$$(awk '/MemTotal/{printf "%d", $$2 * 0.8 / 1024}' /proc/meminfo) \
		--entrypoint /bin/bash \
		ghcr.io/qemus/qemu:7.29 \
		-c 'echo "📦 Creating temporary COW layer..." && \
		    qemu-img create -f qcow2 -F qcow2 -b $(PWD)/$(IMAGE_PATH) /boot.qcow2 && \
		    echo "✅ COW layer created, starting QEMU..." && \
		    exec /usr/bin/tini -s /run/entry.sh'

# Default values for test-openclaw
IMAGE_PATH ?= $(shell ls -t image/output/cai-final-debug-*.qcow2 2>/dev/null | head -1)
KVM ?= N

# Test Trustee locally in container
dev-trustee: generate-secrets init-terraform
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	VSWITCH_CIDR=$$(cd terraform && terraform console 2>/dev/null <<< "var.vswitch_cidr" | tr -d '"') && \
	TRUSTEE_IP=$$(cd terraform && terraform console 2>/dev/null <<< "var.trustee_private_ip" | tr -d '"') && \
	echo "🔐 Starting local Trustee test environment..." && \
	echo "   Network: cai-test-net ($$VSWITCH_CIDR)" && \
	echo "   Trustee IP: $$TRUSTEE_IP" && \
	echo "" && \
	docker network inspect cai-test-net >/dev/null 2>&1 || \
		(echo "📡 Creating Docker network cai-test-net..." && \
		docker network create --subnet=$$VSWITCH_CIDR cai-test-net) && \
	echo "🔐 Preparing Trustee initialization script..." && \
	mkdir -p /tmp/cai-test && \
	cd terraform && terraform console <<< "module.trustee.user_data_content" | awk '!/^(EOT|<<EOT)$$/' > /tmp/cai-test/trustee-init.sh && \
	cd .. && \
	echo "🚀 Starting Trustee container in foreground..." && \
	echo "📊 Connection Info:" && \
	echo "   Trustee URL: http://$$TRUSTEE_IP:8081/api" && \
	echo "   Container: cai-test-trustee" && \
	echo "" && \
	echo "   Press Ctrl+C to stop" && \
	echo "" && \
	docker rm -f -t 0 cai-test-trustee 2>/dev/null || true && \
	docker run --rm -it \
		--name cai-test-trustee \
		--network cai-test-net \
		--ip $$TRUSTEE_IP \
		-p 8081:8081 \
		-v /tmp/cai-test/trustee-init.sh:/usr/local/bin/trustee-init.sh \
		alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest \
		/bin/bash -c "yum install -y tini && chmod +x /usr/local/bin/trustee-init.sh && exec /usr/bin/tini -s bash -- -c 'env DEV_TRUSTEE=1 /usr/local/bin/trustee-init.sh && sleep infinity'"



# Internal function to launch TNG client
# Args: $(1) = MODE (local or remote)
define _tng_launch
	@echo "🔐 Starting TNG Client..."; \
	export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc; \
	GATEWAY_TOKEN=$$(sed 's| //.*||g' secrets/openclaw.json | jq -r '.gateway.auth.token' 2>/dev/null || echo "NOT_FOUND"); \
	if [ "$$GATEWAY_TOKEN" = "NOT_FOUND" ]; then \
		echo "❌ Error: Cannot read gateway token from secrets/openclaw.json"; \
		echo "   Please run: make generate-secrets"; \
		exit 1; \
	fi; \
	if [ "$(1)" = "local" ]; then \
		echo "📍 Mode: Local Test (Private IP)"; \
		TRUSTEE_IP=$$(cd terraform && terraform console 2>/dev/null <<< "var.trustee_private_ip" | tr -d '"'); \
		TRUSTEE_API_URL="http://$$TRUSTEE_IP:8081/api"; \
		OPENCLAW_IP=$$(cd terraform && terraform console 2>/dev/null <<< "var.openclaw_private_ip" | tr -d '"'); \
		OPENCLAW_ADDR="$$OPENCLAW_IP"; \
		DEPLOY_CMD="make dev-trustee"; \
	else \
		echo "📍 Mode: Remote Test (Public IP)"; \
		TRUSTEE_PUBLIC_IP=$$(cd terraform && terraform output -raw trustee_public_ip 2>/dev/null); \
		if [ -z "$$TRUSTEE_PUBLIC_IP" ]; then \
			echo "❌ Error: Cannot get Trustee public IP"; \
			echo "   Please run: make deploy-infra"; \
			exit 1; \
		fi; \
		TRUSTEE_API_URL="http://$$TRUSTEE_PUBLIC_IP:8081/api"; \
		OPENCLAW_PUBLIC_IP=$$(cd terraform && terraform output -raw openclaw_public_ip 2>/dev/null); \
		if [ -z "$$OPENCLAW_PUBLIC_IP" ]; then \
			echo "❌ Error: Cannot get OpenClaw public IP"; \
			echo "   Please run: make deploy-infra"; \
			exit 1; \
		fi; \
		OPENCLAW_ADDR="$$OPENCLAW_PUBLIC_IP"; \
		DEPLOY_CMD="make deploy-infra"; \
	fi; \
	AS_URL="$$TRUSTEE_API_URL/as"; \
	echo "   Attestation Service: $$AS_URL"; \
	echo -n "      Checking availability... "; \
	if ! curl -f -s -o /dev/null --connect-timeout 5 "$$TRUSTEE_API_URL/health"; then \
		echo ""; \
		echo "❌ Error: Cannot reach Trustee at $$AS_URL"; \
		echo "   Please ensure Trustee is running:"; \
		echo "   - Run: $$DEPLOY_CMD"; \
		exit 1; \
	fi; \
	echo "✅ reachable"; \
	echo "   OpenClaw Address: $$OPENCLAW_ADDR:18789"; \
	echo -n "      Checking availability (port 18789)... "; \
	if ! timeout 5 bash -c "cat < /dev/null > /dev/tcp/$$OPENCLAW_ADDR/18789" 2>/dev/null; then \
		echo "❌ Error: Cannot reach OpenClaw at $$OPENCLAW_ADDR:18789"; \
		if [ "$(1)" = "local" ]; then \
			echo "   Please ensure OpenClaw is running:"; \
			echo "   - Run: make dev-openclaw"; \
		else \
			echo "   Please ensure OpenClaw is deployed:"; \
			echo "   - Run: make deploy-openclaw"; \
		fi; \
		exit 1; \
	fi; \
	echo "✅ reachable"; \
	echo ""; \
	TNG_CONFIG=$$(echo "$${TNG_CLIENT_CONFIG_TEMPLATE}" | sed "s|<AS_URL>|$$AS_URL|g; s|<OPENCLAW_ADDR>|$$OPENCLAW_ADDR|g"); \
	echo "📋 TNG Client Configuration:"; \
	echo "$$TNG_CONFIG" | jq . 2>/dev/null || echo "$$TNG_CONFIG"; \
	echo ""; \
	echo "🚀 Launching TNG Client Container..."; \
	echo "   Access Information:"; \
	echo "      OpenClaw Control UI URL:   http://localhost:18789/openclaw"; \
	echo "      OpenClaw Gateway URL:      ws://localhost:18789/ (for TUI/App remote access)"; \
	echo "      OpenClaw Gateway Token:    $$GATEWAY_TOKEN"; \
	echo "" && \
	echo "   Press Ctrl+C to stop" && \
	echo ""; \
	docker rm -f -t 0 cai-tng-client 2>/dev/null || true && \
	docker run -it --rm --privileged \
		--network host \
		--cgroupns=host \
		--name cai-tng-client \
		ghcr.io/inclavare-containers/tng:latest \
		tng launch --config-content="$$TNG_CONFIG"
endef

# Test TNG client locally (use private IP)
dev-tng: init-terraform
	@$(call _tng_launch,local)

# Test TNG client with deployed Trustee (use public IP)
connect-tng: init-terraform
	@$(call _tng_launch,remote)

# Clean up local Trustee test container
clean-dev-trustee:
	@echo "🛑 Stopping local Trustee test container..."
	@docker stop cai-test-trustee 2>/dev/null || echo "   Container not running"
	@rm -f /tmp/cai-test/trustee-init.sh
	@echo "✅ Local Trustee test environment cleaned"

# Clean up local OpenClaw test container
clean-dev-openclaw:
	@echo "🛑 Stopping local OpenClaw test container..."
	@docker stop cai-test-openclaw 2>/dev/null || echo "   Container not running"
	@echo "✅ Local OpenClaw test environment cleaned"

# Clean up TNG client container
clean-dev-tng:
	@echo "🛑 Stopping TNG client container..."
	@docker stop cai-tng-client 2>/dev/null || echo "   Container not running"
	@echo "✅ TNG client cleaned"

# Clean up all local test containers
clean-dev-all: clean-dev-trustee clean-dev-openclaw clean-dev-tng
	@echo "✅ All local test environments cleaned"
