variable "zone_id" {
  type        = string
  default     = "cn-beijing-l"
  description = "Primary availability zone ID (cn-beijing-l for TDX/TEE instances)"
}

variable "vpc_cidr" {
  type        = string
  default     = "10.0.0.0/16"
  description = "VPC CIDR block"
}

variable "vswitch_cidr" {
  type        = string
  default     = "10.0.1.0/24"
  description = "VSwitch CIDR block"
}

# Fixed private IPs for services
variable "trustee_private_ip" {
  type        = string
  default     = "10.0.1.10"
  description = "Trustee instance private IP (must be within vswitch_cidr)"
}

variable "openclaw_private_ip" {
  type        = string
  default     = "10.0.1.20"
  description = "OpenClaw instance private IP (must be within vswitch_cidr)"
}

variable "image_type" {
  type        = string
  default     = "prod"
  description = "Image type to use: 'prod' or 'debug'. Default is 'prod'"
  validation {
    condition     = contains(["prod", "debug"], var.image_type)
    error_message = "image_type must be either 'prod' or 'debug'"
  }
}

variable "security_group_allowed_cidr" {
  type        = string
  default     = "0.0.0.0/0"
  description = "Security Group Allowed CIDR. Controls the source IP range allowed to access: Trustee SSH (port 22), Trustee API (port 8081), OpenClaw SSH (port 22), OpenClaw Gateway (port 18789)"
}

variable "openclaw_instance_type" {
  type        = string
  default     = "ecs.g8i.xlarge"
  description = "OpenClaw instance type (Intel g8i series for TDX, requires ecs.g8i.xlarge or higher)"
}

variable "trustee_instance_type" {
  type        = string
  default     = "ecs.g7.xlarge"
  description = "Trustee ECS instance type"
}

variable "project_name" {
  type        = string
  default     = "cai"
  description = "Project name for resource naming"
}

