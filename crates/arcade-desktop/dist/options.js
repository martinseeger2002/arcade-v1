// Options: automatic updates (off unless the operator turns them on).
const OPT = { busy: false };

function agoText(t) {
  if (!t) return "not yet";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 90) return "just now";
  if (s < 5400) return `${Math.round(s / 60)} minutes ago`;
  return `${Math.round(s / 3600)} hours ago`;
}

async function refreshOptions() {
  const u = await api("update.status");
  $("o-ver").textContent = u.version || "local build";
  $("o-new").textContent = u.newest ? (u.newer ? `${u.newest} (newer)` : `${u.newest} (this one)`) : "—";
  $("o-last").textContent = agoText(u.last_check);
  const auto = $("o-auto");
  if (document.activeElement !== auto) auto.checked = !!u.on;
  auto.disabled = !u.supported || OPT.busy;
  $("o-check").disabled = !u.supported || OPT.busy || u.doing !== "idle";
  $("o-now").disabled = !u.supported || !u.newer || OPT.busy || u.doing !== "idle";
  if (OPT.busy) return;
  if (!u.supported) say("o-msg", `Updates can't be installed by this arcade: ${u.why_not}.`, true);
  else if (u.doing === "restarting") say("o-msg", `Updated to ${u.installed}: restarting…`);
  else if (u.doing !== "idle") say("o-msg", `${u.doing[0].toUpperCase()}${u.doing.slice(1)}…`);
  else if (u.error) say("o-msg", u.error, true);
  else $("o-msg").textContent = "";
}

async function optionsDo(what, f) {
  OPT.busy = true;
  say("o-msg", what);
  try { await f(); OPT.busy = false; await refreshOptions(); }
  catch (e) { OPT.busy = false; say("o-msg", e.message, true); }
}

$("o-auto").addEventListener("change", (e) => optionsDo(e.target.checked ? "Turning automatic updates on…" : "Turning automatic updates off…",
  () => api("update.set", { on: e.target.checked })));
$("o-check").addEventListener("click", () => optionsDo("Looking for a new release…", () => api("update.check")));
$("o-now").addEventListener("click", () => {
  if (!confirm("Install the newest release now? The service restarts; your wallet and data stay as they are.")) return;
  optionsDo("Downloading the new release…", () => api("update.now"));
});

// Use this arcade from a browser (ui.link): the tunnel command and the link, in the app and in a
// browser alike (docs/WEBUI.md).
const WEBUI = { link: null, shown: false };
async function refreshWebUi() {
  try { WEBUI.link = await api("ui.link"); } catch (e) { say("o-web-msg", e.message, true); return; }
  const l = WEBUI.link;
  $("o-tunnel").textContent = l.tunnel;
  $("o-plain").textContent = `http://127.0.0.1:${l.port}/ui/`;
  $("o-link").textContent = WEBUI.shown ? l.url : "hidden";
  $("o-link-show").textContent = WEBUI.shown ? "Hide" : "Show";
}
async function copyText(text, what) {
  try { await navigator.clipboard.writeText(text); say("o-web-msg", `${what} copied.`); }
  catch { say("o-web-msg", `Couldn't copy here: select the ${what.toLowerCase()} and copy it by hand.`, true); WEBUI.shown = true; refreshWebUi(); }
}
$("o-tunnel-copy").addEventListener("click", () => WEBUI.link && copyText(WEBUI.link.tunnel, "Tunnel command"));
$("o-link-copy").addEventListener("click", () => WEBUI.link && copyText(WEBUI.link.url, "Link"));
$("o-link-show").addEventListener("click", () => { WEBUI.shown = !WEBUI.shown; refreshWebUi(); });

// Look: the default, or an installed theme app (theme.js).
(async () => {
  const sel = document.getElementById("o-look"); if (!sel) return;
  const msg = document.getElementById("o-look-msg");
  const fill = async () => {
    let list = [];
    try { list = await THEME.installedThemes(); } catch {}
    const cur = THEME.current();
    sel.replaceChildren(h("option", { value: "" }, "Default look"), ...list.map((a) => h("option", { value: a.id }, a.card.name)));
    sel.value = cur && list.some((a) => a.id === cur.id) ? cur.id : "";
    sel._list = list;
  };
  sel.onchange = async () => {
    msg.className = "msg"; msg.textContent = "";
    if (!sel.value) return THEME.clear();
    const a = sel._list.find((x) => x.id === sel.value);
    try { await THEME.use(a); } catch (e) { msg.className = "msg bad"; msg.textContent = `${a.card.name} can't be worn: ${e.message}`; await fill(); }
  };
  document.getElementById("o-look-store").onclick = () => {
    goTab("apps"); document.querySelector('#ap2-views button[data-v="store"]')?.click();
    let n = 0; const t = setInterval(() => { const c = document.querySelector('#ap2-cats [data-cat="themes"]'); if (c || ++n > 20) { clearInterval(t); c?.click(); } }, 150);
  };
  document.querySelector('nav button[data-tab="options"]')?.addEventListener("click", fill);
  fill();
})();
