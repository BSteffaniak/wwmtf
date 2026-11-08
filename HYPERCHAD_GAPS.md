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
- **Read-only text inputs** (and select-all on focus). The template accepts `readonly` but the text input element drops it. Workaround: the lobby invite link is an unnamed text input outside any form (edits are never submitted), next to a plain anchor.

## Client actions

- **Set/toggle class or data attribute, and set arbitrary style properties** (border, opacity, translate), not only background, display and visibility. This would let ephemeral selection state, such as which rack tile is picked, live client-side without a server round trip. Workaround: every pick is a compose POST; `fx-click` sets an immediate background on the tapped tile or square.
- **Set input value.** Workaround: separate hidden-field forms per option.
- **Element-scoped HTTP lifecycle triggers.** With the vanilla-js `http-events` plugin, a request started without an originating element (shared-state, navigation) dispatches `http-before-request` on the document, and the handler then runs every `fx-http-before-request` action on the page. Per-element pending feedback therefore fires on unrelated requests. Workaround: use `fx-click` on the submit control for immediate pending feedback; the plugin stays disabled.
- **Clipboard write** (copy text). Workaround: invite links shown in selectable read-only fields.

## Server configuration

- **Configurable HTTP server timeouts** (request-head timeout, keep-alive) for every server adapter. The Actix adapter uses the framework defaults, so a connection that sends nothing for about five seconds gets an unsolicited `408 Request Timeout`. Browsers that preconnect a spare socket and use it later can receive that 408 as a page response. Workaround: production fronts the app with nginx (fresh upstream connection per request), and the browser acceptance harness disables Chrome network prediction.

## App shell and responsiveness

- **App metadata:** icon and theme color, mapped to favicon/manifest on HTML and window icon on desktop renderers.
- **Responsive triggers beyond `MaxWidth`/`MaxHeight`** (min-width, orientation), and responsive values without requiring an explicit element id.

## Responsive values

- **Responsive values for any `Number`.** The template only turns integer responsive values (`i32`/`i64`/`f32`) into a base attribute value; `calc(...)`, percent, and viewport units fail to compile, even though override generation already accepts any `Number`. A generic `From<IfExpression<T: Into<Number>, Responsive>> for Number` would fix it, and an app crate can't add it (orphan rule). Workaround: the game scene builds per-layout sizes as one `calc()` with a clamped step term at the breakpoint (`step_length` in `pages/game.rs`).
- **Responsive values must be written inline.** Overrides are only generated when the attribute literally contains `if_responsive(...)`. A helper that returns the same `IfExpression` compiles but silently keeps only the default value. It should either work through the type or fail to compile.
- **`gap` is not responsive.** Workaround: one gap for every layout.

## Overlays and controls

- **Pass-through hit testing for overlay containers** (a renderer-neutral `pointer-events: none` equivalent). A transparent full-width HUD bar blocks taps on the board beneath it. Workaround: every HUD cluster is its own small absolutely positioned element.
- **Form controls inherit typography.** HTML buttons fall back to the browser's control font size instead of their parent's, so text inside board squares needs an explicit `font-size=100%`. Renderers could normalize controls to inherit font family and size.

## Layout and feature wiring

- **Single flex properties imply the others.** Setting only `flex-shrink=0` emits `flex-grow:1; flex-basis:0%`, and `flex="0 0 auto"` emits `flex-basis:0`, so fixed-size items stretch or collapse. Workaround: pin fixed-size items with equal `width` and `min-width` and leave flex unset.
- **Responsive template values are coupled to app logic.** `if_responsive(...)` needs the `logic` feature, which also turns on `hyperchad_app` logic; the app builder then spawns its action listener on the current runtime and panics if there is none. Workaround: build the app inside a dedicated runtime context. A template-only responsive feature would avoid this.
- **Returned page roots are unwrapped.** A route view whose container is the page root element itself renders only its children, so the root's id and styles (for example `#app-page`) disappear. Workaround: return the root inside a wrapper container (`Container::from(vec![root])`).
