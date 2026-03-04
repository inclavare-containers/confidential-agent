# Trustee module - Attestation Service

# Security group for Trustee
resource "alicloud_security_group" "trustee" {
  security_group_name = "${var.project_name}-trustee-sg"
  vpc_id              = var.vpc_id
  description         = "Security group for Trustee attestation service"
}

# Allow SSH (for management)
resource "alicloud_security_group_rule" "trustee_ssh" {
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = "22/22"
  security_group_id = alicloud_security_group.trustee.id
  cidr_ip           = var.security_group_allowed_cidr
}

# Allow Trustee API access (attestation service on port 8081)
# Used for health checks, remote attestation requests, and KBS operations
resource "alicloud_security_group_rule" "trustee_as" {
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = "8081/8081"
  security_group_id = alicloud_security_group.trustee.id
  cidr_ip           = var.security_group_allowed_cidr
}

# Allow Trustee API access from within VPC (for OpenClaw and internal services)
resource "alicloud_security_group_rule" "trustee_as_vpc" {
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = "8081/8081"
  security_group_id = alicloud_security_group.trustee.id
  cidr_ip           = var.vpc_cidr
}

# Local values for secrets - read from files and encode as base64
locals {
  # Derive image reference value path from image path
  image_reference_value_path = "${regex("^(.*)\\.qcow2$", var.image_file_path)[0]}.json"

  user_data_vars = {
    disk_passphrase_base64       = filebase64("${path.root}/../secrets/disk_passphrase")
    sshd_server_key_base64       = filebase64("${path.root}/../secrets/sshd_server_key")
    sshd_server_pub_base64       = filebase64("${path.root}/../secrets/sshd_server_key.pub")
    openclaw_config_base64       = filebase64("${path.root}/../secrets/openclaw.json")
    image_reference_value_base64 = filebase64(local.image_reference_value_path)
  }
  user_data_content = templatefile("${path.module}/user-data.sh.tftpl", local.user_data_vars)
}

# Compute user-data hash to trigger instance replacement when script or secrets change
resource "random_id" "user_data_hash" {
  byte_length = 8
  keepers = {
    user_data_content = local.user_data_content
  }
}

# Trustee ECS instance
resource "alicloud_instance" "trustee" {
  instance_name        = "${var.project_name}-trustee"
  availability_zone    = var.zone_id
  security_groups      = [alicloud_security_group.trustee.id]
  instance_type        = var.instance_type
  image_id             = "aliyun_3_x64_20G_alibase_20260122.vhd"
  vswitch_id           = var.vswitch_id
  private_ip           = var.private_ip
  
  system_disk_category = "cloud_essd"
  system_disk_size     = 40

  internet_max_bandwidth_out = 10
  
  user_data = local.user_data_content

  # Force recreation when user-data changes
  lifecycle {
    replace_triggered_by = [
      random_id.user_data_hash
    ]
  }

  tags = {
    Name    = "${var.project_name}-trustee"
    Project = var.project_name
  }
}

# Wait for Trustee instance to be running (simple dependency)
resource "null_resource" "trustee_instance_ready" {
  depends_on = [alicloud_instance.trustee]
}

# Check Trustee service availability via public IP
resource "null_resource" "check_trustee_health" {
  depends_on = [null_resource.trustee_instance_ready]

  provisioner "local-exec" {
    command = <<EOT
      echo "Checking Trustee service health at ${alicloud_instance.trustee.public_ip}:8081..."
      for i in {1..30}; do
        if timeout 5 curl -sf http://${alicloud_instance.trustee.public_ip}:8081/api/health 2>/dev/null; then
          echo "✓ Trustee service is healthy and accessible!"
          exit 0
        fi
        echo "Attempt $i: Trustee not responding yet, waiting 10 seconds..."
        sleep 10
      done
      echo "❌ Error: Trustee service failed to respond within 5 minutes"
      exit 1
    EOT
  }
}
