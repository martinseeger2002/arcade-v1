// Options: automatic updates (off unless the operator turns them on).
const OPT = { busy: false };

function agoText(t) {
  if (!t) return "not yet";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 90) return "just now";
  if (s < 5400) return `${Math.round(s / 60)} minutes ago`;
  return `${Math.round(s / 3600)} hours ago`;
}

// Storage settings (Martin 2026-11-19): the keep share, the blob cache and what to keep live in the ARCADE Storage app
// (@strawberry's, family "arcade-storage"), which asks for each change through this arcade's approval card
// (POST /r/fog/keep_share, /r/fog/cache). arc.keep and arc.cache stay the operator's commands. Here: the disk line on
// Chains & Coins (information), the Home suggestion, and buttons that open the app at its Settings.
const STORAGE_APP = { family: "arcade-storage", creator: "strawberry", at: "#/settings" };
async function openStorage(msgId) {
  const pick = (l) => (l || []).filter((a) => a.card && a.card.family === STORAGE_APP.family && a.creator_tag === STORAGE_APP.creator)
    .sort((x, y) => (y.height || 0) - (x.height || 0))[0];
  let a;
  try { a = pick(await api("apps.installed")); } catch (e) { say(msgId, e.message, true); return; }
  if (!a) {
    say(msgId, "ARCADE Storage (by @strawberry) isn't installed here: get it in Apps → App Store (search \"ARCADE Storage\"), then open its Settings tab.", true);
    return;
  }
  try {
    const l = await api("apps.launch", { chain: a.chain, txid: a.txid });
    await invoke("open_app", { id: l.id, title: l.title, frame: l.frame, at: STORAGE_APP.at });
    $(msgId).textContent = "";
  } catch (e) { say(msgId, e.message || String(e), true); }
}
$("o-storage-open").addEventListener("click", () => openStorage("o-storage-msg"));
$("c-disk-open").addEventListener("click", () => openStorage("c-disk-msg"));

const gbText = (b) => b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(b >= 10 * (1 << 30) ? 0 : 1)} GB` : b >= 1048576 ? `${(b / 1048576).toFixed(1)} MB` : b > 0 ? `${Math.max(1, Math.round(b / 1024))} kB` : "0";
function diskLine(k) {
  const b = k.budget || {};
  const grow = b.reserve_bytes > 0 ? ` (growing to ${gbText(b.chains_expected_bytes)})` : "";
  const chains = b.kept_chains || b.chains_bytes ? `Chains &amp; Coins uses <b>${gbText(b.chains_bytes)}</b>${grow}` : `Chains &amp; Coins: <b>every chain borrowed</b>`;
  const keep = k.mb ? `ARCADE keep <b>${gbText(k.mb * 1048576)}</b> (holding ${gbText(b.keep_bytes || 0)})` : "ARCADE keep <b>off</b>";
  return `${chains} · ${keep} · <b>${gbText(b.free_bytes || 0)}</b> free`;
}
// The disk line on Chains & Coins (arc.keep, crate::diskbudget): information only; its button opens the Storage app.
async function refreshDisk() {
  let k;
  try { k = await api("arc.keep"); } catch (e) { $("c-disk").textContent = "—"; return; }
  $("c-disk").innerHTML = diskLine(k);
  return k;
}
document.querySelector('nav button[data-tab="chains"]')?.addEventListener("click", refreshDisk);
refreshDisk();

// The suggestion on Home: for an arcade earning ARCADE with 50 GB or more free that keeps nothing. Dismissable for
// good; it only opens the Storage app's keep settings, it never sets a share.
async function keepPrompt() {
  let k;
  try { k = await api("arc.keep"); } catch { $("h-keep").hidden = true; return; }
  $("h-keep").hidden = !k.suggest;
  if (k.suggest) {
    const w = k.would_hold || {};
    $("h-keep-text").textContent = `This arcade earns ARCADE and has ${gbText(k.free_bytes)} free, but keeps none of the chain's files for the network.` +
      (w.short_segments ? ` ${w.short_segments} segment${w.short_segments === 1 ? " is" : "s are"} short of copies right now.` : "") +
      ` Suggested for this machine: ${gbText(k.suggested?.bytes || 0)}, after what your chains will grow to.` +
      " A keep share holds some of them, sealed and paid like your other Earn ARCADE data. Nothing changes unless you choose one in the ARCADE Storage app.";
  }
}
$("h-keep-go").addEventListener("click", () => openStorage("h-keep-msg"));
$("h-keep-no").addEventListener("click", async () => {
  try { await api("arc.keep", { dismiss: true }); } catch {}
  $("h-keep").hidden = true;
});
keepPrompt();
setInterval(keepPrompt, 10 * 60 * 1000);

// Relayed links (mesh.relay): the switch, the month's limit, the meter, and the links passed now.
async function refreshRelay() {
  try {
    const r = await api("mesh.relay");
    $("o-relay-state").textContent = r.on ? (r.over ? "on, limit reached" : "on") : "off";
    $("o-relay-month").textContent = `${r.used_gb} of ${r.limit_gb} GB`;
    $("o-relay-today").textContent = `${r.today_gb} GB`;
    $("o-relay-links").textContent = `${r.passing.length} of ${r.max_links}`;
    if (document.activeElement !== $("o-relay-on")) $("o-relay-on").checked = !!r.on;
    if (document.activeElement !== $("o-relay-gb")) $("o-relay-gb").value = r.limit_gb;
    if (document.activeElement !== $("o-relay-max")) $("o-relay-max").value = r.max_links;
    if (document.activeElement !== $("o-relay-price")) $("o-relay-price").value = r.price_per_gb;
    if (document.activeElement !== $("o-relay-free")) $("o-relay-free").value = r.free_mb;
    $("o-relay-earned").textContent = `${r.earned} ARCADE` + (r.waiting_payments ? ` (${r.waiting_payments} waiting for a block)` : "");
    $("o-relay-payto").textContent = r.pay_to || "no address: free until one is set";
    const list = $("o-relay-list");
    list.replaceChildren(
      ...r.passing.map((c) => h("li", {}, `Passing ${c.a} ⇄ ${c.b}: ${c.mb} MB`)),
      ...r.accounts.map((a) => h("li", {}, `${a.arcade}: sent ${a.used_mb} MB this month, charged ${a.month_charged}, paid ${a.paid}` + (a.owed > 0 ? `, owes ${a.owed}` : "") + " ARCADE")),
      ...r.through_relays.map((c) => h("li", {}, `This arcade reaches ${c.arcade} through ${c.relay}`)));
    if (document.activeElement !== $("o-pay-on")) $("o-pay-on").checked = !!r.pay_on;
    if (document.activeElement !== $("o-pay-cap")) $("o-pay-cap").value = r.pay_cap;
    $("o-pay-list").replaceChildren(
      h("li", {}, `Paid this month: ${r.paid_this_month} of ${r.pay_cap} ARCADE`),
      ...r.bills.map((b) => h("li", {}, `${b.relay}: ${b.price_per_gb} ARCADE a GB, ${b.used_mb} MB sent this month, balance ${b.balance.toFixed(4)} ARCADE`)));
  } catch (e) {
    $("o-relay-state").textContent = "—";
  }
}
$("o-relay-on").addEventListener("change", async (e) => {
  try { await api("mesh.relay_set", { on: e.target.checked }); say("o-relay-msg", e.target.checked ? "Relaying links for other arcades." : "Relay off."); }
  catch (err) { say("o-relay-msg", err.message, true); }
  refreshRelay();
});
$("o-relay-set").addEventListener("click", async () => {
  const limit_gb = Math.max(0, Number($("o-relay-gb").value) || 0);
  const max_links = Math.max(0, Math.floor(Number($("o-relay-max").value) || 0));
  try { await api("mesh.relay_set", { limit_gb, max_links }); say("o-relay-msg", `Limit set: ${limit_gb} GB a month, ${max_links} links at once.`); }
  catch (e) { say("o-relay-msg", e.message, true); }
  refreshRelay();
});

$("o-relay-fee-set").addEventListener("click", async () => {
  const price_per_gb = Math.max(0, Number($("o-relay-price").value) || 0);
  const free_mb = Math.max(0, Math.floor(Number($("o-relay-free").value) || 0));
  try { await api("mesh.relay_set", { price_per_gb, free_mb }); say("o-relay-msg", price_per_gb ? `Price set: ${price_per_gb} ARCADE a GB, ${free_mb} MB free a month.` : "Relaying is free."); }
  catch (e) { say("o-relay-msg", e.message, true); }
  refreshRelay();
});
$("o-relay-payto-set").addEventListener("click", async () => {
  const pay_to = $("o-relay-payto-in").value.trim();
  try { await api("mesh.relay_set", { pay_to }); say("o-relay-msg", pay_to ? `Paid to ${pay_to}.` : "Paid to this arcade's wallet."); }
  catch (e) { say("o-relay-msg", e.message, true); }
  refreshRelay();
});
$("o-pay-on").addEventListener("change", async (e) => {
  try { await api("mesh.relay_set", { pay_on: e.target.checked }); say("o-pay-msg", e.target.checked ? "Paying relays that charge, within the cap." : "Not paying relays."); }
  catch (err) { say("o-pay-msg", err.message, true); }
  refreshRelay();
});
$("o-pay-set").addEventListener("click", async () => {
  const pay_cap = Math.max(0, Number($("o-pay-cap").value) || 0);
  try { await api("mesh.relay_set", { pay_cap }); say("o-pay-msg", `Cap set: ${pay_cap} ARCADE a month.`); }
  catch (e) { say("o-pay-msg", e.message, true); }
  refreshRelay();
});

// Share system info (sysinfo.status / sysinfo.set): the switch, and the exact record it sends, field by field.
const SI_FIELDS = [["cpu", "CPU"], ["cores", "Cores"], ["ram_gb", "RAM"], ["gpus", "GPUs"], ["disk_gb", "Disk (this arcade's data)"],
  ["os", "System"], ["arch", "Architecture"], ["version", "Arcade version"], ["seal", "Seal speed"]];
function siValue(k, v, st) {
  if (k === "cores") return [v.physical != null ? `${v.physical} physical` : null, v.logical != null ? `${v.logical} logical` : null].filter(Boolean).join(", ");
  if (k === "ram_gb") return `${v} GB`;
  if (k === "gpus") return v.map((g) => g.unified ? `${g.name}: memory shared with the RAM` : g.vram_gb != null ? `${g.name}: ${g.vram_gb} GB` : g.name).join("; ");
  if (k === "disk_gb") return `${v.total} GB, ${v.free} GB free`;
  if (k === "seal") return `${v} core-seconds per GiB, measured`;
  return String(v);
}
async function refreshSysinfo() {
  let st;
  try { st = await api("sysinfo.status"); } catch (e) { say("o-si-msg", e.message, true); return; }
  if (document.activeElement !== $("o-si-on")) $("o-si-on").checked = !!st.on;
  const r = st.record || {};
  const rows = SI_FIELDS.map(([k, label]) => {
    let val = r[k] != null ? siValue(k, r[k], st) : null;
    if (k === "seal" && val == null) val = `not measured yet: ${(st.seal.sealed_bytes / 1048576).toFixed(0)} of ${(st.seal.needs_bytes / 1048576).toFixed(0)} MiB sealed here`;
    if (val == null) val = "can't be read here: left out";
    return h("tr", {}, h("td", {}, label), h("td", { translate: ["cpu", "version", "os", "arch"].includes(k) ? "no" : null }, val),
      h("td", { class: "muted" }, k === "seal" ? "measured" : "reported"));
  });
  $("o-si-rows").replaceChildren(...rows);
  const sig = JSON.stringify(st.record);
  if ($("o-si-json").dataset.sig !== sig) { $("o-si-json").dataset.sig = sig; $("o-si-json").textContent = JSON.stringify(st.record, null, 1); }
  if (!$("o-si-msg").textContent) say("o-si-msg", st.on ? (st.sent_at ? `Shared: last sent ${agoText(st.sent_at)}.` : "Shared.") : `Not shared. This is what would go out (${st.bytes} bytes, plus its signature).`);
}
$("o-si-on").addEventListener("change", async (e) => {
  try {
    await api("sysinfo.set", { on: e.target.checked });
    say("o-si-msg", e.target.checked ? "Sharing this arcade's system info with the mesh." : "Not shared any more: every arcade was told to drop it.");
  } catch (err) { say("o-si-msg", err.message, true); }
  refreshSysinfo();
});

// Litecoin MWEB (mweb.status / .set): off by default; on only after the operator accepts the notice.
let MW_NOTICE = null;
async function refreshMweb() {
  let st;
  try { st = await api("mweb.status"); } catch (e) { say("o-mw-msg", e.message, true); return; }
  MW_NOTICE = st;
  if (document.activeElement !== $("o-mw-on")) $("o-mw-on").checked = !!st.on;
  $("o-mw-notice").textContent = st.notice;
  const ltc = (v) => v == null ? "—" : `${(v / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 })} LTC`;
  $("o-mw-rows").replaceChildren(...(st.pools || []).map((p) => h("tr", {},
    h("td", {}, p.name), h("td", {}, `MWEB pool ${ltc(p.pool)} at block ${p.height}`),
    h("td", { class: "muted" }, `last block: ${p.pegin ? ltc(p.pegin) + " in" : "no peg-ins"}, ${p.pegout ? ltc(p.pegout) + " out" : "no peg-outs"}, ${p.kernels} kernels`))));
  if (!$("o-mw-msg").textContent) say("o-mw-msg", st.on ? (st.pools && st.pools.length ? "MWEB on." : "MWEB on. No Litecoin chain with MWEB blocks is installed here yet.") : "MWEB off.");
}
$("o-mw-on").addEventListener("change", async (e) => {
  const on = e.target.checked;
  const body = { on };
  if (on) {
    const st = MW_NOTICE || await api("mweb.status");
    if (!confirm(`${st.notice}\n\nAccept this notice and turn MWEB on?`)) { e.target.checked = false; return; }
    body.accept = st.notice_version;
  }
  try {
    await api("mweb.set", body);
    say("o-mw-msg", on ? "MWEB on. You accepted the notice." : "MWEB off.");
  } catch (err) { say("o-mw-msg", err.message, true); }
  refreshMweb();
});

// Content screening (screening.status / .set / .test / .topup): off (the default), borrowing within a daily cap, or the operator's own model.
async function refreshScreening() {
  let st;
  try { st = await api("screening.status"); } catch (e) { say("o-scr-msg", e.message, true); return; }
  if (document.activeElement !== $("o-scr-mode")) $("o-scr-mode").value = st.mode;
  if (document.activeElement !== $("o-scr-unchecked")) $("o-scr-unchecked").value = st.unchecked;
  $("o-scr-borrow").hidden = st.mode !== "borrow";
  $("o-scr-own").hidden = st.mode !== "own";
  const cur = st.borrow.current;
  $("o-scr-state").textContent = st.mode === "off" ? "Off" : st.working ? "Working" : `Not working: ${st.why}`;
  $("o-scr-state").className = "v " + (st.mode === "off" || !st.working ? "bad" : "ok");
  $("o-scr-model-in-use").textContent = st.mode === "own" ? (st.own.model || st.own.url || "—") : cur ? `${cur.model} at ${cur.who}` : "—";
  $("o-scr-cost").textContent = st.mode === "own" ? "free (your own)" : cur ? (cur.here ? "free (this arcade's own)" : `${ARC(cur.per_text)} a text, ${ARC(cur.per_picture)} a picture`) : "—";
  $("o-scr-credit").textContent = st.mode === "borrow" && cur ? (cur.credit != null ? String(cur.credit) : "asking…") : "—";
  $("o-scr-today").textContent = `${ARC(st.spent_today)} of ${ARC(st.day_cap)} ARCADE`;
  const c = st.checked;
  $("o-scr-counts").textContent = `${c.ok} ok, ${c.sensitive} covered, ${c.illegal} refused${c.waiting ? `, ${c.waiting} waiting` : ""}`;
  if (document.activeElement !== $("o-scr-cap")) $("o-scr-cap").value = st.day_cap / 1e8;
  if (document.activeElement !== $("o-scr-url") && !$("o-scr-url").value) $("o-scr-url").value = st.own.url || "";
  if (document.activeElement !== $("o-scr-model") && !$("o-scr-model").value) $("o-scr-model").value = st.own.model || "";
  $("o-scr-key").placeholder = st.own.key_set ? "(kept: type to change)" : "(none)";
  const testLine = (t) => !t ? "not tested yet" : t.ok ? `passed the test ${agoText(t.at)}` : `failed the test ${agoText(t.at)}: ${t.error || `${t.passed} of ${t.of} right`}`;
  $("o-scr-offers").replaceChildren(...(st.borrow.offers.length ? st.borrow.offers.map((o) => h("li", {},
    h("b", { translate: "no" }, o.model), ` at ${o.who}${o.here ? " (this arcade)" : ""}: `,
    o.here ? "free" : `about ${ARC(o.per_text)} ARCADE a text, ${ARC(o.per_picture)} a picture`,
    `; ${o.state}; ${testLine(o.test)}${cur && cur.model === o.model && cur.arcade === o.arcade ? " (in use)" : ""}.`))
    : [h("li", {}, st.borrow.last_look ? "No arcade offers a screening model right now." : "Looking at the mesh's offers…")]));
  if (st.mode === "own" && st.own.test && !$("o-scr-msg").dataset.kept) say("o-scr-msg", `Your model ${testLine(st.own.test)}.`, !st.own.test.ok);
}
const scrSet = async (body, said) => {
  try { await api("screening.set", body); say("o-scr-msg", said); }
  catch (e) { say("o-scr-msg", e.message, true); }
  refreshScreening();
};
$("o-scr-mode").addEventListener("change", (e) => {
  const m = e.target.value;
  if (m === "off" && !confirm("Turn screening off? Pictures and text from the chains are then shown unchecked, here and to anyone who can reach this arcade's pages, including content that may be illegal.")) { refreshScreening(); return; }
  if (m === "own" && !$("o-scr-url").value.trim()) { $("o-scr-borrow").hidden = true; $("o-scr-own").hidden = false; say("o-scr-msg", "Give your model's address, then Save and test."); return; }
  scrSet(m === "own" ? { mode: "own", url: $("o-scr-url").value.trim(), model: $("o-scr-model").value.trim() } : { mode: m },
    m === "borrow" ? "Borrowing the cheapest screening model that passes the test." : m === "off" ? "Screening is off." : "Using your own model; testing it.");
});
$("o-scr-save").addEventListener("click", async () => {
  const body = { mode: "own", url: $("o-scr-url").value.trim(), model: $("o-scr-model").value.trim() };
  if ($("o-scr-key").value) body.key = $("o-scr-key").value;
  say("o-scr-msg", "Saving and testing your model…");
  try {
    await api("screening.set", body);
    $("o-scr-key").value = "";
    const st = await api("screening.test");
    const t = st.own.test;
    say("o-scr-msg", t && t.ok ? "Your model passed the test: screening uses it now." : `Your model didn't pass: ${t ? t.error || `${t.passed} of ${t.of} right` : st.why}`, !(t && t.ok));
  } catch (e) { say("o-scr-msg", e.message, true); }
  refreshScreening();
});
$("o-scr-test").addEventListener("click", async () => {
  say("o-scr-msg", "Testing…");
  try { const st = await api("screening.test"); say("o-scr-msg", st.working ? "Screening works." : `Not working: ${st.why}`, !st.working); }
  catch (e) { say("o-scr-msg", e.message, true); }
  refreshScreening();
});
$("o-scr-topup").addEventListener("click", async () => {
  const amount = Math.round(Number($("o-scr-amt").value) * 1e8);
  try { const r = await api("screening.topup", { amount }); say("o-scr-msg", `Paid ${amount / 1e8} ARCADE of screening credit to ${r.to}. It counts once in a block (about 2.5 minutes).`); }
  catch (e) { say("o-scr-msg", e.message, true); }
  refreshScreening();
});
$("o-scr-cap-set").addEventListener("click", () => scrSet({ day_cap: Math.round(Number($("o-scr-cap").value) * 1e8) }, "Daily screening budget set."));
$("o-scr-unchecked").addEventListener("change", (e) => scrSet({ unchecked: e.target.value },
  e.target.value === "cover" ? "When screening isn't working, unchecked content is covered everywhere." : "When screening isn't working, unchecked content shows on this computer and is covered for others."));

// How much of this computer the arcade uses (cpu.status / cpu.set; Martin 2026-11-20: "we need to chill out a bit").
async function refreshCpu() {
  let st;
  try { st = await api("cpu.status"); } catch (e) { say("o-cpu-msg", e.message, true); return; }
  if (document.activeElement !== $("o-cpu-level")) $("o-cpu-level").value = st.level;
  $("o-cpu-warn").hidden = !st.unthrottled;
  if (!$("o-cpu-msg").dataset.kept) say("o-cpu-msg", cpuLine(st));
}
// One line on the level in force: the cap, the arcade's use now, and whether heavy work is holding back for it.
function cpuLine(st) {
  const prio = `${st.nice ? ", below your own programs" : ""}${st.nice_from_env != null ? " (priority set by ARCADE_NICE)" : ""}`;
  const now = `Using about ${st.used_percent || 0}% now`;
  if (st.unthrottled) return `Unthrottled: every core (${st.all_cores}), no cap. ${now}.`;
  return `At most ${st.cap_percent}% of this computer's CPU (${st.cores} of ${st.all_cores} cores for heavy work)${prio}. ${now}${st.holding_back ? ", holding back to stay under the cap" : ""}.`;
}
$("o-cpu-level").addEventListener("change", async (e) => {
  if (e.target.value === "full" && !confirm("Unthrottled lets the arcade use every core, up to 100% of the CPU while chains sync. Recommended only for machines dedicated to running an arcade. Turn the cap off?")) {
    refreshCpu();
    return;
  }
  try {
    const r = await api("cpu.set", { level: e.target.value });
    $("o-cpu-msg").dataset.kept = "1";
    $("o-cpu-warn").hidden = !r.unthrottled;
    say("o-cpu-msg", r.applied_now ? `Now: ${cpuLine(r)}` : `Kept. ${r.note || ""}`);
  } catch (err) { say("o-cpu-msg", err.message, true); }
  setTimeout(() => { delete $("o-cpu-msg").dataset.kept; refreshCpu(); }, 4000);
});

// Welcoming new arcades (faucet.newcomers / faucet.welcome_set): automatic welcomes and their amount.
async function refreshWelcomeOpts() {
  try {
    const w = await api("faucet.newcomers");
    if (document.activeElement !== $("o-wel-auto")) $("o-wel-auto").checked = !!w.auto;
    if (document.activeElement !== $("o-wel-amount")) $("o-wel-amount").value = w.amount / 1e8;
  } catch (e) { /* not on this arcade */ }
}
$("o-wel-auto").addEventListener("change", async (e) => {
  try { await api("faucet.welcome_set", { auto: e.target.checked }); say("o-wel-msg", e.target.checked ? "New arcades are welcomed automatically after people's head start." : "Automatic welcomes off: Home still shows new arcades with the button."); }
  catch (err) { say("o-wel-msg", err.message, true); }
  refreshWelcomeOpts();
});
$("o-wel-set").addEventListener("click", async () => {
  const amount = Math.round(Number($("o-wel-amount").value) * 1e8);
  try { const r = await api("faucet.welcome_set", { amount }); say("o-wel-msg", `Each new arcade gets ${r.amount / 1e8} ARCADE from this arcade.`); }
  catch (e) { say("o-wel-msg", e.message, true); }
  refreshWelcomeOpts();
});

async function refreshOptions() {
  refreshCpu();
  refreshScreening();
  refreshDisk();
  refreshRelay();
  refreshWelcomeOpts();
  refreshSysinfo();
  refreshMweb();
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
// The code of the release an update would install (Arcade V1.0's releases are the read-only repository
// @arcade/arcade-v1, relrepo.rs), in Arcade Git: Strawberry's app, installed from the App Store first if it isn't here.
const CODE_REPO = "arcade/arcade-v1";
async function openCode() {
  const git = (l) => (l || []).filter((a) => a.card && a.card.family === "arcade-git" && a.creator_tag === "strawberry")
    .sort((x, y) => (y.height || 0) - (x.height || 0))[0];
  let a = git(await api("apps.installed"));
  if (!a) {
    a = git((await api("apps.store", { q: "arcade git" })).apps);
    if (!a) throw new Error("Arcade Git isn't in the App Store on this arcade yet: the code is also at /git/@arcade/arcade-v1 for git clone");
    if (!confirm("Arcade Git (by @strawberry) isn't installed here. Install it from the App Store to read the code?")) return;
    await api("apps.install", { chain: a.chain, txid: a.txid });
  }
  // The repository's main is the newest release this arcade holds: the one Update now would install.
  const l = await api("apps.launch", { chain: a.chain, txid: a.txid });
  await invoke("open_app", { id: l.id, title: l.title, frame: l.frame, at: `#/@${CODE_REPO}` });
}
$("o-code").addEventListener("click", () => openCode().catch((e) => say("o-msg", e.message || String(e), true)));
$("o-check").addEventListener("click", () => optionsDo("Looking for a new release on the network…", () => api("update.check")));
$("o-now").addEventListener("click", () => {
  if (!confirm("Install the newest release now? The service restarts; your wallet and data stay as they are.")) return;
  optionsDo("Installing the new release…", () => api("update.now"));
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
