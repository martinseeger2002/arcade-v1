# Themes

A theme changes how the window looks and is laid out, never what it does. It is an app in the App
Store's **Themes** category, published like any app: an HTML page inscribed with an app card. Nothing
about themes is built into a release except the small piece that wears one (`crates/arcade-desktop/dist/theme.js`).

## Wearing one

- **Apps → App Store → Themes**: *Preview* opens the theme's page (sandboxed, like any app);
  *Use theme* checks it, installs it and wears it.
- **Options → Look**: the default look or any installed theme.
- In a theme with menus: **View** lists the looks; on a desktop, the **start menu** does.

The choice is kept by the window itself (localStorage): the desktop app and each browser choose for
themselves. A theme follows its publisher's updates the way an installed app does: the newest version
is worn from the next start. On a screen narrower than 700 px a desktop theme isn't worn (the default
look is).

## What a theme is

The app's page carries its theme in a script block the page itself never runs:

```html
<script type="application/arcade-theme+json">
{
  "arcade_theme": 1,
  "name": "Windows 98",
  "layout": "desktop",
  "colors": {
    "light": { "bg": "#ffffff", "panel": "#c0c0c0", "ink": "#000000", "muted": "#404040",
               "line": "#808080", "accent": "#000080", "accent-ink": "#ffffff" },
    "dark":  { "...": "optional, the same keys" }
  },
  "font": "tahoma", "size": 12, "radius": 0,
  "brand": "Arcade",
  "desktop": { "bar": "bottom", "dock": "none", "topbar": "none", "start": "Start",
               "icons": true, "clock": true, "buttons": "right" },
  "window": { "toolbar": true },
  "icons": { "wallet": "data:image/png;base64,...", "start": "data:image/svg+xml;base64,..." },
  "chrome": ".desk{background:#008080} .win.active .titlebar{background:linear-gradient(90deg,#000080,#1084d0)}"
}
</script>
```

The card is an ordinary app card with `"category": "themes"`, e.g.
`{"app": {"name": "Windows 98", "category": "themes", "version": "1.0", "description": "..."}}`.

| Field | What it sets |
|---|---|
| `layout` | `"window"`: one application window with a menu bar (File, Settings, Window, Go, View), a toolbar of the screens and a status bar. `"desktop"`: every screen in a window of its own on a desktop. |
| `colors` | The screens' colours: `bg panel panel-2 ink muted line accent accent-ink accent-soft warn-bg warn-line bad faint` (hex). `bg panel ink muted line accent accent-ink` are needed. |
| `font`, `size`, `radius` | `system tahoma ubuntu mac serif mono`; 11–16 px; 0–16 px corners. |
| `brand` | Text in the menu bar and at the head of the start menu. |
| `desktop` | `bar` (taskbar: bottom, top, none), `dock` (none, left, bottom), `topbar` (none; `clock`: a bar with the start button and a clock; `menus`: a menu bar), `start` (its label), `icons` (on the desktop), `clock`, `buttons` (a window's buttons at the left or right). |
| `icons` | Pictures for `home wallet messages feed apps explorer chains mine activity apitab options start`, as base64 `data:image/png`, `gif`, `webp` or `svg+xml` (under 12,000 characters each). Without them, plain line icons. |
| `chrome` | CSS for the frame only (below). |

The page must be under 64 KB: the window reads it as the Explorer does (`explorer.ledger_inscription`).

## What a theme can't do

A theme comes from a stranger, and the window it dresses holds a wallet. So it is **checked before it is
worn**, and a theme that fails isn't worn at all (the reason is shown):

- **Readable**: `ink` on `bg`, `panel` and `warn-bg`, and `accent-ink` on `accent`, at least 4.5:1;
  `muted` and `bad` on `panel` at least 3:1 (the original arcade's rule for its themes).
- **The frame's CSS reaches the frame only.** The frame is drawn in a shadow root, and the screens are
  placed into it from outside, so no selector in it can name a screen. Its selectors may use only the
  frame's own class names: `frame layout-window layout-desktop menubar mbitem menu mitem msep check
  toolbar tbitem ticon statusbar sync links brand wbody desk icons icon icon-img icon-label win active
  max titlebar title ticn tbtns tbtn tmin tmax tclose bar bar-top bar-bottom start tasks task on tray
  clock dock dock-left dock-bottom ditem startmenu smhead smitem topbar chain conn selected`, plus
  `:hover :active :focus-visible :first-child :last-child :not(.x)`. There are no element, id, attribute,
  `:host` or `::slotted` selectors, and no pseudo-elements.
- **Properties**: colours, backgrounds, borders, corners, shadows, fonts, spacing, `gap`, `outline`,
  `cursor`, `line-height`, `transition`. Sizes (`width`, `height`, and their min and max) are allowed
  only on bars, buttons and icons. Nothing that moves, hides or covers anything is allowed: no
  `position`, `display`, `opacity`, `visibility`, `transform`, `filter`, `clip`, `content`,
  `z-index`, `!important`, negative margins, or lengths over 64 px. `backdrop-filter` is allowed
  as `blur()` only.
- **Nothing from outside**: pictures only as base64 `data:` images in a background; no `@import`,
  `@font-face`, `@keyframes` or escapes. `@media (prefers-color-scheme: …)` is the only at-rule.
- The screens set their own text colour, font, size, spacing and line height, so nothing the frame
  inherits changes what a screen shows: an address, an amount, a send confirmation.

A theme has no script that runs in the window. Its page runs sandboxed only when someone presses Preview.
