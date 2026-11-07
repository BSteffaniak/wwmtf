# Generic HyperChad response capability

HyperChad revision `43738fed825779c90a6937f9b72fa9af388c17eb` adds renderer-neutral `ResponseMetadata`, secure/expiring `ResponseCookie` mutations, and redirect effects to `View`/`Content` responses. HTML/Actix, generic web-server, and Lambda adapters translate those effects to HTTP response cookies/headers without application-owned renderer code.

Words with More Than Friends account POST handlers parse renderer-neutral forms, create/revoke durable opaque sessions, append secure session/CSRF cookies, expire them on logout, and redirect through those generic response effects. No game-owned Actix route or custom JavaScript cookie path is used.

# Open generic UI capability gaps

The UI redesign stays within current HyperChad primitives (template attributes, `fx-*` actions, responsive triggers) and uses no stylesheets or raw HTML. These capabilities would improve every renderer, not only HTML; each entry notes the workaround wwmtf uses today.

## Styling primitives

- **Elevation / shadow** (offset, blur, spread, color, or a semantic elevation level). Workaround: thicker darker bottom/right borders to fake raised tiles and cards.
- **Gradient backgrounds** (linear/radial color stops). Workaround: flat colors with layered bordered containers.
- **Transform scale and rotate.** Only `translate-x`/`translate-y` exist. Workaround: translate for "lifted" selected tiles; no tilt or pop.
- **Property transitions/animation** (duration, easing on opacity/translate/background; renderers may snap when unsupported). Workaround: none; state changes are instant.
- **Stacking order (`z-index`).** Workaround: rely on document order for overlays and sheets.
- **Typography:** `line-height`, `letter-spacing`, `text-transform`. Workaround: uppercase literals and padding.
- **`aspect-ratio`.** Workaround: equal `calc(...)` width and height expressions for board squares and rack tiles.
- **Font assets.** `font-family` names a family but nothing registers bundled font files generically. Workaround: system font stacks.
- **Vector icons** (a renderer-neutral `svg`/path element). Workaround: text glyphs and letter tiles.

## Interaction state

- **Declarative state variants** (hover, pressed, focused, disabled) declared in the template and implemented natively per renderer. Today `fx-hover` is the only option and pressed/focus have no reset. Workaround: server-rendered selected states and `fx-hover` background changes.
- **Pointer drag and drop triggers** (drag start/over/drop with element data). Workaround: tap tile, then tap square.

## Client actions

- **Set/toggle class or data attribute, and set arbitrary style properties** (border, opacity, translate), not only background, display and visibility. This would let ephemeral selection state, such as which rack tile is picked, live client-side without a server round trip. Workaround: every pick is a compose POST; `fx-http-before-request` gives immediate background feedback.
- **Set input value.** Workaround: separate hidden-field forms per option.
- **Clipboard write** (copy text). Workaround: invite links shown in selectable read-only fields.

## App shell and responsiveness

- **App metadata:** icon and theme color, mapped to favicon/manifest on HTML and window icon on desktop renderers.
- **Responsive triggers beyond `MaxWidth`/`MaxHeight`** (min-width, orientation), and responsive values without requiring an explicit element id.
