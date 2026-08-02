resource "cloudflare_dns_record" "app" {
  zone_id = local.zone_id
  name    = local.app_hostname
  type    = "AAAA"
  content = var.fly_ipv6_address
  proxied = true
  ttl     = 1
  comment = "Words with Spouses canonical application hostname"
}

resource "cloudflare_dns_record" "fly_ownership" {
  zone_id = local.zone_id
  name    = "_fly-ownership.${local.app_hostname}"
  type    = "TXT"
  content = var.fly_ownership_txt
  proxied = false
  ttl     = 3600
  comment = "Fly certificate domain ownership verification"
}

resource "cloudflare_zone_setting" "ssl" {
  zone_id    = local.zone_id
  setting_id = "ssl"
  value      = "strict"
}

resource "cloudflare_turnstile_widget" "registration" {
  count      = var.manage_turnstile ? 1 : 0
  account_id = var.cloudflare_account_id
  domains    = [local.app_hostname]
  mode       = "managed"
  name       = "WWMTF public registration"
  region     = "world"
}
