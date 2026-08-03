variable "cloudflare_api_token" {
  type        = string
  sensitive   = true
  description = "One-time Cloudflare token with account R2 bucket write access"
}

variable "cloudflare_account_id" {
  type        = string
  description = "Cloudflare account that owns the private R2 buckets"
}

variable "state_bucket_name" {
  type        = string
  description = "Globally unique private R2 bucket used for OpenTofu state"
}

variable "backup_bucket_name" {
  type        = string
  description = "Globally unique private R2 bucket used for encrypted database backups"
}

variable "bucket_location" {
  type        = string
  default     = "enam"
  description = "R2 location hint for both buckets"

  validation {
    condition     = contains(["apac", "eeur", "enam", "weur", "wnam", "oc"], var.bucket_location)
    error_message = "bucket_location must be an R2 location hint supported by the Cloudflare provider."
  }
}

variable "backup_retention_days" {
  type        = number
  default     = 180
  description = "Days to retain encrypted off-Fly database backup objects"

  validation {
    condition     = var.backup_retention_days >= 60
    error_message = "backup_retention_days must be at least 60 days."
  }
}

variable "state_archive_retention_days" {
  type        = number
  default     = 365
  description = "Days to retain immutable encrypted OpenTofu state revisions under history/"

  validation {
    condition     = var.state_archive_retention_days >= 90
    error_message = "state_archive_retention_days must be at least 90 days."
  }
}
