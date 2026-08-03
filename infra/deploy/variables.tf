variable "cloudflare_api_token" {
  type        = string
  sensitive   = true
  description = "Cloudflare token scoped to R2 management and required hyperchad.dev zone resources"
}

variable "cloudflare_account_id" {
  type        = string
  description = "Cloudflare account containing hyperchad.dev"
}

variable "zone_name" {
  type        = string
  default     = "hyperchad.dev"
  description = "Existing Cloudflare zone"
}

variable "app_subdomain" {
  type        = string
  default     = "wwmtf"
  description = "Canonical application subdomain"
}

variable "fly_app_name" {
  type        = string
  default     = "wwmtf"
  description = "Fly application hostname prefix"
}

variable "fly_ipv6_address" {
  type        = string
  description = "Dedicated Fly IPv6 address discovered during production bootstrap"
}

variable "fly_ownership_txt" {
  type        = string
  description = "Fly certificate ownership verification token discovered during production bootstrap"
}

variable "manage_redirect_ruleset" {
  type        = bool
  default     = true
  description = "Manage the shared dynamic redirect phase after guarded preflight adoption"
}

variable "manage_firewall_ruleset" {
  type        = bool
  default     = false
  description = "Manage the shared custom firewall phase after importing or coordinating existing rules"
}

variable "manage_managed_waf_ruleset" {
  type        = bool
  default     = false
  description = "Manage the shared managed WAF phase after importing or coordinating existing rules"
}

variable "manage_rate_limit_ruleset" {
  type        = bool
  default     = false
  description = "Manage the shared rate-limit phase after importing or coordinating existing rules"
}

variable "manage_cache_ruleset" {
  type        = bool
  default     = false
  description = "Manage the shared cache phase after importing or coordinating existing rules"
}

variable "manage_security_headers_ruleset" {
  type        = bool
  default     = false
  description = "Manage the shared response-header transform phase after importing or coordinating existing rules"
}

variable "manage_turnstile" {
  type        = bool
  default     = false
  description = "Create the registration Turnstile widget; app integration must be deployed before enabling it"
}

variable "cloudflare_managed_ruleset_id" {
  type        = string
  default     = null
  nullable    = true
  description = "Optional Cloudflare-managed WAF ruleset ID available to the zone plan; discover it via the Cloudflare API before enabling"
}
