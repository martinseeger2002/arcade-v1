# The app's window in a browser (/ui/)

For an arcade on a computer without a screen, or one you sit away from: arcaded serves the desktop
app's own window (the files of `crates/arcade-desktop/dist`, built into arcaded) at `/ui/` on its
web port. The browser shows the same tabs, apps and windows as the app; nothing is copied or redone.

## Use it

**In the app:** Options → *Use this arcade from a browser* shows the steps, this computer's tunnel
command (its user and LAN address filled in) and the link (hidden until you press Show), each with
a Copy button. The same section is there in the browser window, so a new key after a restart is
one click away in either.

**In a terminal**, on the arcade's computer:

```sh
arcade web
```

prints a link such as `http://127.0.0.1:8420/ui/#k=…`. Open it in a browser there.

From another computer, open an SSH tunnel first, with **the same port on both ends** (the arcade
answers only when it's asked for by its own port), then open the same link there:

```sh
ssh -N -L 8420:127.0.0.1:8420 you@arcade-computer
```

The browser keeps the key, so the plain address `http://127.0.0.1:8420/ui/` works afterwards, until
the arcade restarts (it then asks for the new link).

## What it lets in

- **Nothing new.** The window's calls go to `/rpc`, arcaded's JSON-RPC: the cookie (`<home>/.cookie`,
  new at each start) as Basic auth, the commands the app sends over the socket. The link carries
  the cookie's secret; the page keeps it in this browser and sends it with each call.
- **This computer only**, for `/ui/` as for `/rpc`: a loopback peer asking for this port by a
  loopback name, with no proxy's headers, never with `--behind-proxy`. An SSH tunnel counts (it
  arrives on loopback); `--http-bind 0.0.0.0` doesn't open the window to the network. Another
  website's page can't call `/rpc` (the Authorization header needs a preflight the arcade doesn't
  allow).
- With the key, the window can do everything the app can (wallet included): treat the link like
  the wallet's words while it's valid.
- The files are served with the app's own content rules (CSP), `X-Frame-Options: DENY` and
  `frame-ancestors 'none'`, so no other page can frame the window.

## How it works

`common.js` is the one place the window talks to the app (`invoke`). Outside the app (no
`window.__TAURI__`) it uses `webInvoke` instead:

| Call | In the app | In a browser |
|---|---|---|
| `api` | the app forwards `{cmd, ...}` to arcaded's local socket | `POST /rpc` `{method: cmd, params}` with the cookie |
| `open_app` | a new app window (`appwindow.html?frame=…`) | the same page in a pop-up window |
| `open_content` | a content window | the same address in a pop-up |
| `save_file` | written to Downloads | the browser's download |

So every command the window sends must be one `/rpc` takes (`crates/arcaded/src/commands.rs`); a
test (`webui.rs`) reads the window's scripts and fails when one isn't. `GET /ui/` and its files are
http.rs `web_ui` with webui.rs; `ui.link` on the socket (`arcade web`) gives the link.
