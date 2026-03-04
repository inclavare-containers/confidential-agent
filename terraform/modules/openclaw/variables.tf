variable "project_name" {
  type = string
}

variable "vpc_id" {
  type = string
}

variable "vswitch_id" {
  type = string
}

variable "zone_id" {
  type = string
}

variable "instance_type" {
  type    = string
  default = "ecs.ebmgn8i.32xlarge"
}

variable "private_ip" {
  type    = string
  default = ""
  description = "Fixed private IP for the instance (optional, leave empty for auto-assign)"
}

variable "image_id" {
  type        = string
  description = "Custom OpenClaw image ID"
}

variable "trustee_url" {
  type = string
}

variable "security_group_allowed_cidr" {
  type        = string
  default     = "0.0.0.0/0"
  description = "Security Group Allowed CIDR for OpenClaw SSH (port 22) and Gateway (port 18789)"
}
