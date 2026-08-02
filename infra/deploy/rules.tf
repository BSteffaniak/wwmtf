resource "cloudflare_ruleset" "redirects" {
  count       = var.manage_zone_rulesets ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF redirects"
  description = "Redirect the hyperchad.dev games path to the canonical WWMTF hostname"
  kind        = "zone"
  phase       = "http_request_dynamic_redirect"

  rules = [{
    action = "redirect"
    action_parameters = {
      from_value = {
        status_code = 301
        target_url = {
          expression = "concat(\"https://${local.app_hostname}\", substring(http.request.uri.path, 12))"
        }
        preserve_query_string = true
      }
    }
    expression  = "http.host eq \"${var.zone_name}\" and (http.request.uri.path eq \"/games/wwmtf\" or starts_with(http.request.uri.path, \"/games/wwmtf/\"))"
    description = "Redirect /games/wwmtf to the canonical application hostname"
    enabled     = true
    ref         = "wwmtf_path_redirect"
  }]
}

resource "cloudflare_ruleset" "firewall" {
  count       = var.manage_zone_rulesets ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF custom firewall"
  description = "Hostname-scoped request filtering for WWMTF"
  kind        = "zone"
  phase       = "http_request_firewall_custom"

  rules = [
    {
      action      = "block"
      expression  = "http.host eq \"${local.app_hostname}\" and not http.request.method in {\"GET\" \"HEAD\" \"POST\" \"OPTIONS\"}"
      description = "Reject methods unused by the application"
      enabled     = true
      ref         = "wwmtf_methods"
    },
    {
      action      = "block"
      expression  = "http.host eq \"${local.app_hostname}\" and http.request.uri.path in {\"/.env\" \"/.git/config\" \"/wp-admin\" \"/wp-login.php\" \"/xmlrpc.php\"}"
      description = "Block common secret and CMS scanner paths"
      enabled     = true
      ref         = "wwmtf_scanners"
    },
  ]
}

resource "cloudflare_ruleset" "managed_waf" {
  count       = var.manage_zone_rulesets && var.cloudflare_managed_ruleset_id != null ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF managed WAF"
  description = "Execute the Cloudflare-managed WAF ruleset selected for the zone plan"
  kind        = "zone"
  phase       = "http_request_firewall_managed"

  rules = [{
    action = "execute"
    action_parameters = {
      id = var.cloudflare_managed_ruleset_id
    }
    expression  = "http.host eq \"${local.app_hostname}\""
    description = "Execute managed WAF rules for WWMTF"
    enabled     = true
    ref         = "wwmtf_managed_waf"
  }]
}

resource "cloudflare_ruleset" "rate_limits" {
  count       = var.manage_zone_rulesets ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF rate limits"
  description = "Protect public account endpoints from automated abuse"
  kind        = "zone"
  phase       = "http_ratelimit"

  rules = [{
    action = "managed_challenge"
    ratelimit = {
      characteristics     = ["cf.colo.id", "ip.src"]
      period              = 10
      requests_per_period = 5
      mitigation_timeout  = 10
      requests_to_origin  = true
    }
    expression  = "http.host eq \"${local.app_hostname}\" and http.request.method eq \"POST\" and http.request.uri.path in {\"/login\" \"/register\"}"
    description = "Challenge repeated login and registration attempts"
    enabled     = true
    ref         = "wwmtf_account_rate_limit"
  }]
}

resource "cloudflare_ruleset" "cache" {
  count       = var.manage_zone_rulesets ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF cache policy"
  description = "Never cache private or dynamic application responses"
  kind        = "zone"
  phase       = "http_request_cache_settings"

  rules = [{
    action = "set_cache_settings"
    action_parameters = {
      cache = false
    }
    expression  = "http.host eq \"${local.app_hostname}\""
    description = "Bypass cache for WWMTF"
    enabled     = true
    ref         = "wwmtf_cache_bypass"
  }]
}

resource "cloudflare_ruleset" "security_headers" {
  count       = var.manage_zone_rulesets ? 1 : 0
  zone_id     = local.zone_id
  name        = "WWMTF security headers"
  description = "Set conservative browser and privacy response headers"
  kind        = "zone"
  phase       = "http_response_headers_transform"

  rules = [{
    action = "rewrite"
    action_parameters = {
      headers = {
        "Cache-Control" = {
          operation = "set"
          value     = "private, no-store"
        }
        "Content-Security-Policy" = {
          operation = "set"
          value     = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
        }
        "Permissions-Policy" = {
          operation = "set"
          value     = "camera=(), geolocation=(), microphone=()"
        }
        "Referrer-Policy" = {
          operation = "set"
          value     = "no-referrer"
        }
        "Strict-Transport-Security" = {
          operation = "set"
          value     = "max-age=31536000"
        }
        "X-Content-Type-Options" = {
          operation = "set"
          value     = "nosniff"
        }
        "X-Frame-Options" = {
          operation = "set"
          value     = "DENY"
        }
      }
    }
    expression  = "http.host eq \"${local.app_hostname}\""
    description = "Set WWMTF response security headers"
    enabled     = true
    ref         = "wwmtf_security_headers"
  }]
}
