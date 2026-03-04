variable "project_name" {
  type = string
}

variable "vpc_id" {
  type = string
}

variable "vpc_cidr" {
  type        = string
  description = "VPC CIDR block for internal access rules"
}

variable "vswitch_id" {
  type = string
}

variable "zone_id" {
  type = string
}

variable "private_ip" {
  type    = string
  default = "10.0.1.10"
  description = "Fixed private IP for Trustee instance"
}

variable "instance_type" {
  type    = string
  default = "ecs.g7.xlarge"
}

variable "security_group_allowed_cidr" {
  type        = string
  default     = "0.0.0.0/0"
  description = "Security Group Allowed CIDR for Trustee SSH (port 22) and API (port 8081)"
}

variable "image_file_path" {
  type        = string
  description = "Path to the VM image file (qcow2). Image reference value JSON will be derived by changing suffix to .json"
}
