output "instance_ids" {
  description = "OpenClaw instance IDs"
  value       = alicloud_instance.openclaw.id
}

output "private_ip" {
  description = "OpenClaw instance private IP"
  value       = alicloud_instance.openclaw.private_ip
}

output "public_ip" {
  description = "OpenClaw instance public IP"
  value       =  alicloud_instance.openclaw.public_ip
}
