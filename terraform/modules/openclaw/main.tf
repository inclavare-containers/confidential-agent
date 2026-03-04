# OpenClaw module - Confidential computing instance

# Security group for OpenClaw instances
resource "alicloud_security_group" "openclaw" {
  security_group_name = "${var.project_name}-openclaw-sg"
  vpc_id              = var.vpc_id
  description         = "Security group for OpenClaw confidential computing instances"
}

# Allow SSH (for management)
resource "alicloud_security_group_rule" "openclaw_ssh" {
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = "22/22"
  security_group_id = alicloud_security_group.openclaw.id
  cidr_ip           = var.security_group_allowed_cidr
}

# Allow OpenClaw gateway port (18789) - protected by TNG
resource "alicloud_security_group_rule" "openclaw_gateway" {
  type              = "ingress"
  ip_protocol       = "tcp"
  port_range        = "18789/18789"
  security_group_id = alicloud_security_group.openclaw.id
  cidr_ip           = var.security_group_allowed_cidr
}

# OpenClaw instance
resource "alicloud_instance" "openclaw" {
  instance_name        = "${var.project_name}-openclaw"
  availability_zone    = var.zone_id
  security_groups      = [alicloud_security_group.openclaw.id]
  instance_type        = var.instance_type
  image_id             = var.image_id
  vswitch_id           = var.vswitch_id
  # Use fixed IP for single instance deployment (OpenClaw)
  private_ip           = var.private_ip != "" ? var.private_ip : null

  system_disk_category = "cloud_essd"
  system_disk_size     = 200

  # Enable public IP for remote access
  internet_max_bandwidth_out = 10

  security_options {
    confidential_computing_mode = "TDX"
  }

  # Note: We should not use cloud-init/user-data since cloud-init is disabled in image

  tags = {
    Name    = "${var.project_name}-openclaw"
    Project = var.project_name
  }
}
