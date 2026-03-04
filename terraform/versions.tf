terraform {
  required_version = ">= 1.0"

  required_providers {
    alicloud = {
      source  = "aliyun/alicloud"
      version = ">= 1.200.0"
    }
    random = {
      source  = "hashicorp/random"
      version = ">= 3.0.0"
    }
  }
}

provider "alicloud" {
  region = "cn-beijing"

  endpoints {
    oss = "cn-beijing.oss.aliyuncs.com"
  }
}
