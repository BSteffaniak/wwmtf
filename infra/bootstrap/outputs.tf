output "state_bucket_name" {
  value       = cloudflare_r2_bucket.state.name
  description = "Private R2 bucket for OpenTofu state"
}

output "backup_bucket_name" {
  value       = cloudflare_r2_bucket.backups.name
  description = "Private R2 bucket for age-encrypted database backups"
}

output "r2_endpoint" {
  value       = "https://${var.cloudflare_account_id}.r2.cloudflarestorage.com"
  description = "S3-compatible R2 endpoint used by OpenTofu and the backup workflow"
}

output "backend_hcl" {
  value = <<-EOT
    bucket                      = "${cloudflare_r2_bucket.state.name}"
    key                         = "words-with-spouses/production.tfstate"
    region                      = "auto"
    endpoint                    = "https://${var.cloudflare_account_id}.r2.cloudflarestorage.com"
    use_lockfile                = true
    skip_credentials_validation = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    skip_s3_checksum            = true
    skip_metadata_api_check     = true
  EOT

  description = "Non-secret backend configuration for infra/deploy/backend.hcl"
}
