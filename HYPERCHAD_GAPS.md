# Generic HyperChad response capability

HyperChad revision `43738fed825779c90a6937f9b72fa9af388c17eb` adds renderer-neutral `ResponseMetadata`, secure/expiring `ResponseCookie` mutations, and redirect effects to `View`/`Content` responses. HTML/Actix, generic web-server, and Lambda adapters translate those effects to HTTP response cookies/headers without application-owned renderer code.

Words with More Than Friends account POST handlers parse renderer-neutral forms, create/revoke durable opaque sessions, append secure session/CSRF cookies, expire them on logout, and redirect through those generic response effects. No game-owned Actix route or custom JavaScript cookie path is used.
