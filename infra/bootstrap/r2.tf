resource "cloudflare_r2_bucket" "state" {
  account_id    = var.cloudflare_account_id
  name          = var.state_bucket_name
  location      = var.bucket_location
  storage_class = "Standard"

  lifecycle {
    prevent_destroy = true
  }
}

resource "cloudflare_r2_bucket_lock" "state" {
  account_id  = var.cloudflare_account_id
  bucket_name = cloudflare_r2_bucket.state.name

  rules = [{
    id      = "retain-opentofu-state-history"
    enabled = true
    prefix  = "history/"
    condition = {
      type            = "Age"
      max_age_seconds = var.state_archive_retention_days * 86400
    }
  }]
}

resource "cloudflare_r2_bucket_lifecycle" "state" {
  account_id  = var.cloudflare_account_id
  bucket_name = cloudflare_r2_bucket.state.name

  rules = [
    {
      id      = "expire-old-opentofu-state-history"
      enabled = true
      conditions = {
        prefix = "history/"
      }
      delete_objects_transition = {
        condition = {
          type    = "Age"
          max_age = var.state_archive_retention_days * 86400
        }
      }
    },
    {
      id      = "abort-incomplete-state-uploads"
      enabled = true
      conditions = {
        prefix = ""
      }
      abort_multipart_uploads_transition = {
        condition = {
          type    = "Age"
          max_age = 86400
        }
      }
    }
  ]
}

resource "cloudflare_r2_bucket" "backups" {
  account_id    = var.cloudflare_account_id
  name          = var.backup_bucket_name
  location      = var.bucket_location
  storage_class = "Standard"

  lifecycle {
    prevent_destroy = true
  }
}

resource "cloudflare_r2_bucket_lock" "backups" {
  account_id  = var.cloudflare_account_id
  bucket_name = cloudflare_r2_bucket.backups.name

  rules = [{
    id      = "retain-encrypted-backups"
    enabled = true
    prefix  = ""
    condition = {
      type            = "Age"
      max_age_seconds = var.backup_retention_days * 86400
    }
  }]
}

resource "cloudflare_r2_bucket_lifecycle" "backups" {
  account_id  = var.cloudflare_account_id
  bucket_name = cloudflare_r2_bucket.backups.name

  rules = [
    {
      id      = "expire-old-encrypted-backups"
      enabled = true
      conditions = {
        prefix = ""
      }
      delete_objects_transition = {
        condition = {
          type    = "Age"
          max_age = var.backup_retention_days * 86400
        }
      }
    },
    {
      id      = "abort-incomplete-backup-uploads"
      enabled = true
      conditions = {
        prefix = ""
      }
      abort_multipart_uploads_transition = {
        condition = {
          type    = "Age"
          max_age = 86400
        }
      }
    }
  ]
}
