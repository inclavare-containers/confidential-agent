# Main Terraform configuration for cai demo

# Random suffix for unique bucket name
resource "random_string" "bucket_suffix" {
  length  = 8
  special = false
  upper   = false
}

# OSS bucket for storing cai images
resource "alicloud_oss_bucket" "cai" {
  bucket = "${var.project_name}-images-${random_string.bucket_suffix.result}"

  tags = {
    Name    = "${var.project_name}-images"
    Project = var.project_name
  }
}

# Set bucket ACL using separate resource (deprecated in provider v1.220.0)
resource "alicloud_oss_bucket_acl" "cai" {
  bucket = alicloud_oss_bucket.cai.bucket
  acl    = "private"
}

# Find latest built images using fileset
locals {
  image_dir = "${path.root}/../image/output"

  # Find all timestamped prod/debug images
  prod_images  = fileset(local.image_dir, "cai-final-prod-*.qcow2")
  debug_images = fileset(local.image_dir, "cai-final-debug-*.qcow2")

  # Get latest (highest timestamp = lexically last)
  latest_prod_image  = length(local.prod_images) > 0 ? reverse(sort(tolist(local.prod_images)))[0] : null
  latest_debug_image = length(local.debug_images) > 0 ? reverse(sort(tolist(local.debug_images)))[0] : null

  # Select image based on user preference
  selected_image = var.image_type == "prod" ? local.latest_prod_image : local.latest_debug_image
}

# Upload selected image to OSS (only if exists)
resource "alicloud_oss_bucket_object" "cai_selected_image" {
  count  = local.selected_image != null ? 1 : 0
  bucket = alicloud_oss_bucket.cai.bucket
  key    = "images/${local.selected_image}"
  source = "${local.image_dir}/${local.selected_image}"

  lifecycle {
    # Detect changes via filename (key), not content hash
    ignore_changes = [source]
  }
}

# Import cai image from OSS
# NOTE: First-time import requires ECS service role authorization:
#   1. Go to Alibaba Cloud Console -> ECS -> Images -> Import Image
#   2. Follow the prompts to authorize ECS service account to access OSS
#   3. Or run: aliyun ram CreateServiceLinkedRole --ServiceName ecs.aliyuncs.com
resource "alicloud_image_import" "cai" {
  count = local.selected_image != null ? 1 : 0

  image_name   = replace(local.selected_image, ".qcow2", "")
  description  = "CAI confidential computing image (${var.image_type})"
  os_type      = "linux"
  platform     = "Aliyun"
  architecture = "x86_64"
  boot_mode    = "UEFI"

  disk_device_mapping {
    oss_bucket      = alicloud_oss_bucket.cai.bucket
    oss_object      = alicloud_oss_bucket_object.cai_selected_image[0].key
    disk_image_size = 30
  }

  features {
    nvme_support = "supported"
  }

  timeouts {
    create = "30m"
  }

  depends_on = [alicloud_oss_bucket_object.cai_selected_image]
}

# Determine which image ID to use
locals {
  # Use imported image
  openclaw_image_id = length(alicloud_image_import.cai) > 0 ? alicloud_image_import.cai[0].id : null

  # Full path to selected image for reference value derivation
  selected_image_file_path = local.selected_image != null ? "${local.image_dir}/${local.selected_image}" : ""
}

# VPC for the demo
resource "alicloud_vpc" "cai" {
  vpc_name   = "${var.project_name}-vpc"
  cidr_block = var.vpc_cidr
}

# VSwitch - Primary zone
resource "alicloud_vswitch" "cai" {
  vswitch_name = "${var.project_name}-vsw"
  vpc_id       = alicloud_vpc.cai.id
  cidr_block   = var.vswitch_cidr
  zone_id      = var.zone_id
}

# Trustee module
module "trustee" {
  source = "./modules/trustee"

  project_name                  = var.project_name
  vpc_id                        = alicloud_vpc.cai.id
  vpc_cidr                      = var.vpc_cidr
  vswitch_id                    = alicloud_vswitch.cai.id
  zone_id                       = var.zone_id
  instance_type                 = var.trustee_instance_type
  private_ip                    = var.trustee_private_ip
  security_group_allowed_cidr   = var.security_group_allowed_cidr
  image_file_path               = local.selected_image_file_path

  providers = {
    alicloud = alicloud
  }

}

# OpenClaw single node instance (depends on Trustee being ready)
module "openclaw" {
  source = "./modules/openclaw"

  project_name                  = var.project_name
  vpc_id                        = alicloud_vpc.cai.id
  vswitch_id                    = alicloud_vswitch.cai.id
  zone_id                       = var.zone_id
  instance_type                 = var.openclaw_instance_type
  image_id                      = local.openclaw_image_id
  private_ip                    = var.openclaw_private_ip
  security_group_allowed_cidr   = var.security_group_allowed_cidr
  trustee_url                   = module.trustee.private_url

  # Ensure OpenClaw waits for Trustee to be ready
  depends_on = [module.trustee.wait_for_trustee]

  providers = {
    alicloud = alicloud
  }
}
