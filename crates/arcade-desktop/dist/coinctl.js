// Coin control (arcaded's coinctl): with Options → Wallet → Coin control on, the Wallet's Send panel lists the wallet's
// coins (wallet.coins): tick the ones a payment spends (wallet.send's coins) and freeze the ones to keep
// (wallet.freeze). Tick nothing and the wallet picks, as always. Coins holding an inscription or token can't be ticked.
// A how-to tip shows the first times until "Don't show this again"; the "?" brings it back. The setting and the tip's
// choice are kept per account by the arcade, so the desktop app and the browser /ui/ agree.
const CC = { prefs: null, on: false, list: null, key: "", picked: new Set(), err: "", tipShown: false };

// The setting, read (or changed with `set`) at the arcade; the Options switches follow it.
async function ccPrefs(set) {
  try { CC.prefs = await api("wallet.coin_prefs", set || {}); }
  catch (e) { if (set) throw e; return CC.prefs; }
  CC.on = !!CC.prefs.coin_control;
  $("o-cc").checked = CC.on;
  $("o-cc-tip").checked = !!CC.prefs.tip;
  return CC.prefs;
}

// What the list is read with: the same from / kind / all_kinds the payment is sent with, so it lists the coins that
// payment could spend.
function ccArgs() {
  const ch = picked(), w = cur(), a = { chain: ch.key };
  if (!w) return a;
  a.from = w.address;
  if (w.own && w.akind && w.kinds.length > 1) Object.assign(a, { kind: w.akind.id, all_kinds: $("w-allkinds").checked });
  return a;
}

// The coins picked for wallet.send (none: the wallet picks).
const ccPicked = () => (CC.on && !$("w-cc").hidden && CC.picked.size ? [...CC.picked] : null);

const ccApplies = () => CC.on && !$("w-send-panel").hidden && $("w-asset").value === picked().key && !!cur() && (cur().own || cur().spendable);

async function ccRefresh(force) {
  if (!CC.prefs) await ccPrefs();
  const box = $("w-cc");
  if (!ccApplies()) { box.hidden = true; return; }
  box.hidden = false;
  if (!CC.tipShown && CC.prefs && CC.prefs.tip) { CC.tipShown = true; $("w-cc-tip").hidden = false; }
  const args = ccArgs(), key = JSON.stringify(args), bal = cur().balance;
  // The same wallet and balance: the list stands (redrawn only when something changes). A new balance (a coin came or
  // went) reads it again, keeping the picks still spendable; another wallet, address type or chain starts afresh.
  if (!force && key === CC.key && bal === CC.bal && (CC.list || CC.err)) return;
  if (key !== CC.key) { CC.picked.clear(); CC.list = null; }
  CC.key = key; CC.bal = bal; CC.err = "";
  ccDraw();
  try {
    const r = await api("wallet.coins", args);
    if (CC.key !== key) return;
    CC.list = r;
    // A pick of a coin no longer spendable (spent, frozen) drops.
    for (const id of [...CC.picked]) if (!r.coins.some((c) => c.id === id && c.spendable)) CC.picked.delete(id);
  } catch (e) { if (CC.key === key) CC.err = e.message; }
  ccDraw();
}

// A coin's age: from its block's time, else its confirmations.
function ccAge(c) {
  const conf = c.confirmations ? `${num(c.confirmations)} confirmation${c.confirmations === 1 ? "" : "s"}` : "not in a block yet";
  if (c.age_secs == null) return conf;
  const s = c.age_secs, a = s < 5400 ? `${Math.max(1, Math.round(s / 60))} min old` : s < 172800 ? `${Math.round(s / 3600)} hours old` : `${Math.round(s / 86400)} days old`;
  return `${a} · ${conf}`;
}

function ccRow(c) {
  const ch = picked(), on = CC.picked.has(c.id);
  const box = h("input", { type: "checkbox", "aria-label": `Spend this coin of ${coins(c.value, ch.key)}` });
  box.checked = on; box.disabled = !c.spendable;
  box.addEventListener("change", () => { if (box.checked) CC.picked.add(c.id); else CC.picked.delete(c.id); ccChanged(); });
  const badges = [];
  if (c.status === "inscription") badges.push(h("span", { class: "cc-badge ins", title: "This coin holds an inscription or token, so it is never spent as plain money. Send it from NFTs or Tokens." }, "inscription · locked"));
  else if (c.status === "unchecked") badges.push(h("span", { class: "cc-badge ins", title: "A small coin nobody could check for an inscription yet (inscriptions travel on coins of 0.01 or less): kept back to be safe." }, "may hold an inscription · locked"));
  if (c.frozen) badges.push(h("span", { class: "cc-badge ice", title: "Frozen: no payment spends it until you unfreeze it." }, "frozen"));
  const fz = c.status === "free"
    ? h("button", { class: "small", title: c.frozen ? "Let payments spend this coin again" : "Keep this coin: no payment spends it until you unfreeze it", onclick: () => ccFreeze(c, !c.frozen) }, c.frozen ? "Unfreeze" : "Freeze")
    : h("span");
  return h("div", { class: "cc-row" + (on ? " on" : "") + (c.spendable ? "" : " locked"), title: `${c.address || ""}\n${c.id}` },
    box,
    h("span", { class: "amt" }, coins(c.value, ch.key)),
    h("span", { class: "where" }, shortAddr(c.address || c.txid), c.kind ? ` · ${c.kind}` : "", ...badges),
    h("span", { class: "when" }, ccAge(c)),
    fz);
}

function ccDraw() {
  const list = $("w-cc-list");
  $("w-cc-clear").hidden = !CC.picked.size;
  $("w-cc-mode").textContent = CC.picked.size ? "" : "· none ticked: the wallet picks for you";
  if (CC.err) { list.replaceChildren(h("div", { class: "x-empty bad" }, CC.err)); $("w-cc-sum").replaceChildren(); return; }
  if (!CC.list) { list.replaceChildren(h("div", { class: "x-empty" }, "Reading your coins…")); $("w-cc-sum").replaceChildren(); return; }
  const cs = CC.list.coins || [];
  list.replaceChildren(...(cs.length ? cs.map(ccRow) : [h("div", { class: "x-empty" }, "No coins to spend on this address yet.")]));
  ccSum();
}

// The fee as the arcade works it out (arcade_wallet::tx::fee_for_size, estimate_vsize_coins), for the coins ticked
// and the amount typed: what Review then builds, give or take the length of the address paid.
function ccFee(picks) {
  const l = CC.list, segwit = picks.some((c) => c.vbytes !== 148);
  const vsize = 10 + picks.reduce((a, c) => a + c.vbytes, 0) + (segwit ? 1 : 0) + 2 * (9 + l.change_len);
  return l.per_started_kb ? (Math.floor(vsize / 1000) + 1) * l.fee_per_kb : Math.ceil(vsize * l.fee_per_kb / 1000);
}

function ccSum() {
  const ch = picked(), l = CC.list, sum = $("w-cc-sum");
  const cs = l.coins || [], picks = cs.filter((c) => CC.picked.has(c.id));
  const free = cs.filter((c) => c.spendable).reduce((a, c) => a + c.value, 0);
  const kept = cs.filter((c) => !c.spendable).length;
  const keptLine = kept ? ` · ${kept} coin${kept === 1 ? "" : "s"} locked or frozen` : "";
  if (!picks.length) { sum.replaceChildren(h("span", {}, `${cs.length} coin${cs.length === 1 ? "" : "s"}, ${coins(free, ch.key)} free to spend${keptLine}.`)); return; }
  const total = picks.reduce((a, c) => a + c.value, 0), fee = ccFee(picks);
  const amt = toSats($("w-amt").value);
  const parts = [h("b", {}, `${picks.length} coin${picks.length === 1 ? "" : "s"} picked: ${coins(total, ch.key)}`), ` · fee about ${coins(fee, ch.key)}`];
  if (amt) {
    const change = total - amt - fee;
    if (change < 0) parts.push(" · ", h("span", { class: "bad" }, `not enough: ${coins(-change, ch.key)} short. Tick more coins or send less.`));
    else if (change < l.dust) parts.push(` · no change (${coins(change, ch.key)} left is too small to send back, so it goes to the fee)`);
    else parts.push(` · change about ${coins(change, ch.key)} back to ${shortAddr(l.from)}`);
  } else parts.push(" · type an amount to see the change");
  sum.replaceChildren(...parts);
}

function ccChanged() {
  $("w-confirm").hidden = true; pendingSend = null;
  ccDraw();
}

async function ccFreeze(c, on) {
  try { await api("wallet.freeze", { chain: picked().key, coins: [c.id], frozen: on }); CC.picked.delete(c.id); }
  catch (e) { msg($("w-send-msg"), e.message, "bad"); return; }
  ccChanged(); ccRefresh(true);
}

// The tip: "Got it" closes it for now, "Don't show this again" for good (Options brings it back); "?" opens it.
$("w-cc-help").addEventListener("click", () => { $("w-cc-tip").hidden = !$("w-cc-tip").hidden; });
$("w-cc-tipok").addEventListener("click", () => { $("w-cc-tip").hidden = true; });
$("w-cc-tipoff").addEventListener("click", async () => {
  $("w-cc-tip").hidden = true;
  try { await ccPrefs({ tip: false }); msg($("w-send-msg"), "The tip won't show again. The ? beside Coins to spend opens it any time.", "ok"); } catch (e) { msg($("w-send-msg"), e.message, "bad"); }
});
$("w-cc-clear").addEventListener("click", () => { CC.picked.clear(); ccChanged(); });
$("w-amt").addEventListener("input", () => { if (CC.list && !$("w-cc").hidden) ccSum(); });
// Opening Send reads the setting again (another account may be in use); the list follows the wallet's card (drawCard).
$("w-btn-send").addEventListener("click", async () => { await ccPrefs(); ccRefresh(); });
for (const id of ["w-asset", "w-allkinds"]) $(id).addEventListener("change", () => ccRefresh());
document.querySelector('nav button[data-tab="options"]')?.addEventListener("click", () => ccPrefs());

// Options → Wallet.
$("o-cc").addEventListener("change", async (e) => {
  try {
    await ccPrefs({ coin_control: e.target.checked });
    say("o-cc-msg", CC.on ? "Coin control on: Wallet → Send lists your coins to pick from." : "Coin control off: the wallet picks the coins of each payment.");
    if (CC.on) CC.tipShown = false;
    ccRefresh();
  } catch (err) { e.target.checked = !e.target.checked; say("o-cc-msg", err.message, true); }
});
$("o-cc-tip").addEventListener("change", async (e) => {
  try { await ccPrefs({ tip: e.target.checked }); if (e.target.checked) CC.tipShown = false; say("o-cc-msg", e.target.checked ? "The tip shows again the next time you send." : "The tip is off; the ? beside Coins to spend opens it."); }
  catch (err) { e.target.checked = !e.target.checked; say("o-cc-msg", err.message, true); }
});
ccPrefs();
