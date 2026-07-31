# Generic HyperChad response capability

Words with Spouses can parse renderer-neutral account forms and its durable account/session workflows can create and revoke opaque sessions. Coordinated HyperChad changes now add renderer-neutral `ResponseMetadata`, secure/expiring `ResponseCookie` mutations, and redirect effects to `View`/`Content` responses. HTML/Actix, generic web-server, and Lambda adapters translate those effects to HTTP response cookies/headers without application-owned renderer code.

The application has secure sign-in/sign-out response constructors and account POST handlers that parse renderer-neutral forms, create/revoke durable sessions, append secure session/CSRF cookies, expire them on logout, and redirect through generic response effects. No game-owned Actix route or custom JavaScript cookie path is used.
