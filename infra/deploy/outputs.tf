output "app_url" {
  value       = "https://${local.app_hostname}"
  description = "Canonical application URL"
}

output "fly_origin" {
  value       = "https://${local.fly_hostname}"
  description = "Fly origin used by the proxied DNS record"
}

output "turnstile_sitekey" {
  value       = var.manage_turnstile ? cloudflare_turnstile_widget.registration[0].sitekey : null
  description = "Registration widget site key when Turnstile management is enabled"
}

output "turnstile_secret" {
  value       = var.manage_turnstile ? cloudflare_turnstile_widget.registration[0].secret : null
  sensitive   = true
  description = "Registration widget secret; place it in a secret manager, never source control"
}
