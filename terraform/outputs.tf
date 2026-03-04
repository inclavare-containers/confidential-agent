output "vpc_id" {
  description = "VPC ID"
  value       = alicloud_vpc.cai.id
}

output "openclaw_image_oss_bucket" {
  description = "OSS bucket for cai images"
  value       = alicloud_oss_bucket.cai.bucket
}

output "openclaw_image" {
  description = "Uploaded image path in OSS (based on image_type)"
  value       = length(alicloud_oss_bucket_object.cai_selected_image) > 0 ? alicloud_oss_bucket_object.cai_selected_image[0].key : null
}

output "openclaw_image_id" {
  description = "OpenClaw image ID (imported or custom)"
  value       = local.openclaw_image_id
}

output "trustee_private_ip" {
  description = "Trustee instance private IP"
  value       = module.trustee.private_ip
}

output "trustee_public_ip" {
  description = "Trustee instance public IP"
  value       = module.trustee.public_ip
}

output "trustee_private_url" {
  description = "Trustee service private URL"
  value       = module.trustee.private_url
}

output "trustee_public_url" {
  description = "Trustee service public URL"
  value       = module.trustee.public_url
}

output "openclaw_private_ip" {
  description = "OpenClaw instance private IP"
  value       = module.openclaw.private_ip
}

output "openclaw_public_ip" {
  description = "OpenClaw instance public IP"
  value       = module.openclaw.public_ip
}
