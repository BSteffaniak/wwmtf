terraform {
  required_version = ">= 1.10.0"

  backend "s3" {
    use_lockfile = true
  }

  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "5.8.2"
    }
  }
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}

data "cloudflare_zones" "hyperchad" {
  account = {
    id = var.cloudflare_account_id
  }
  name      = var.zone_name
  max_items = 1
}

locals {
  zone_id      = one(data.cloudflare_zones.hyperchad.result).id
  app_hostname = "${var.app_subdomain}.${var.zone_name}"
  fly_hostname = "${var.fly_app_name}.fly.dev"
}
