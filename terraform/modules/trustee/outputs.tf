output "private_ip" {
  description = "Trustee instance private IP"
  value       = alicloud_instance.trustee.private_ip
}

output "public_ip" {
  description = "Trustee instance public IP"
  value       = alicloud_instance.trustee.public_ip
}

output "private_url" {
  description = "Trustee service private URL"
  value       = "http://${alicloud_instance.trustee.private_ip}:8081/api"
}

output "public_url" {
  description = "Trustee service public URL"
  value       = "http://${alicloud_instance.trustee.public_ip}:8081/api"
}

output "instance_id" {
  description = "Trustee instance ID"
  value       = alicloud_instance.trustee.id
}

output "wait_for_trustee" {
  description = "Resource that checks Trustee service health via public IP"
  value       = null_resource.check_trustee_health
}

output "user_data_content" {
  value       = local.user_data_content
  description = "The computed user data content"
}
