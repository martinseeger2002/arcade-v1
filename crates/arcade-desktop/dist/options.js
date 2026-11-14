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
