variable "cloudflare_api_token" {
  type        = string
  sensitive   = true
  description = "Cloudflare token scoped to the hyperchad.dev zone and Turnstile account resources"
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
  default     = "words-with-spouses"
  description = "Fly application hostname prefix"
}

variable "fly_ipv6_address" {
  type        = string
  default     = "2a09:8280:1::15d:94b3:0"
  description = "Dedicated Fly IPv6 address used as the proxied Cloudflare origin"
}

variable "fly_ownership_txt" {
  type        = string
  default     = "onx2065"
  description = "Fly certificate ownership verification token"
}

variable "manage_zone_rulesets" {
  type        = bool
  default     = false
  description = "Manage shared zone-phase rulesets after importing or coordinating existing hyperchad.dev rules"
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
