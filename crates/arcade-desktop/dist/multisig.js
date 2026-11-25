/* Wallet → Vaults: 2-of-3 multisig vaults, watch-only for now (docs/MULTISIG.md phase 4; arcaded msigsvc.rs).
 * Make a vault from this arcade's key and two others, or add one from its descriptor; see its addresses, coins and
 * balance. Spending with co-signers comes in a later phase. Test chains only. */
const MS = { open: null, form: null, key: null, keyFor: "", coins: {} };

function msTest(ch) { return /test$/.test(ch.key); }

async function drawVaults(ch) {
  const box = $("w-vaults");
  if (!msTest(ch)) {
    box.replaceChildren(h("p", { class: "wl-hint" }, `Multisig vaults are on test chains only for now. Pick a test chain (Pepecoin testnet, Litecoin testnet) to try one.`));
    return;
  }
  const list = await api("multisig.list", { chain: ch.key }).catch((e) => ({ error: e.message }));
  if (picked().key !== ch.key || W.tab !== "vaults") return;
  const tk = TICKERS[ch.key] || ch.name;
  const rows = list.error ? [h("p", { class: "msg bad" }, list.error)] : list.map((v) => vaultRow(ch, v, tk));
  box.replaceChildren(
    h("p", { class: "wl-hint" }, "A vault needs 2 of its 3 keys to spend: for example this arcade, the app on your phone and a paper phrase. Each co-signer keeps their own 12 words and the vault's descriptor."),
    h("div", { class: "wl-rows" }, ...(rows.length ? rows : [h("div", { class: "x-empty" }, `No vaults on ${ch.name} yet.`)])),
    h("div", { class: "row" },
      h("button", { class: "primary", onclick: () => { MS.form = MS.form === "new" ? null : "new"; drawVaults(ch); } }, "New vault"),
      h("button", { onclick: () => { MS.form = MS.form === "add" ? null : "add"; drawVaults(ch); } }, "Add from a descriptor")),
    MS.form === "new" ? newVaultForm(ch) : MS.form === "add" ? addVaultForm(ch) : null,
    h("p", { class: "msg", id: "ms-msg" }));
}

function vaultRow(ch, v, tk) {
  const open = MS.open === v.id;
  const row = h("div", { class: "wl-row click", onclick: () => { MS.open = open ? null : v.id; drawVaults(ch); } },
    h("span", { class: "ic" }, "2/3"),
    h("div", { class: "mid" }, h("div", {}, v.name || "Vault"),
      h("div", { class: "s" }, v.signs_here ? "This wallet holds one of its keys" : "Watch-only: none of its keys is here", ` · ${v.wrap}`)),
    h("span", { class: "amt" }, v.balance == null ? "—" : `${coinNum(v.balance)} ${tk}`));
  if (!open) return row;
  return h("div", {}, row, vaultDetail(ch, v, tk));
}

function vaultDetail(ch, v, tk) {
  const addrBox = h("div", {}), coinsBox = h("div", { class: "wl-rows" });
  const showAddr = (a) => addrBox.replaceChildren(
    h("p", { class: "wl-hint" }, `Receive address ${a.index}: send ${tk} here.`),
    h("div", { class: "row" }, h("code", {}, a.address), h("button", { class: "small", onclick: () => navigator.clipboard.writeText(a.address) }, "Copy")),
    qrImg(a.address));
  showAddr({ index: 0, address: v.first_address });
  api("multisig.coins", { id: v.id }).then((c) => {
    coinsBox.replaceChildren(...(c.coins.length ? c.coins.map((x) => h("div", { class: "wl-row" }, h("span", { class: "ic in" }, "↓"),
      h("div", { class: "mid" }, h("div", { translate: "no" }, shortAddr(x.address)), h("div", { class: "s" }, `${x.change ? "change" : "receive"} ${x.index} · ${x.height ? "block " + x.height : "waiting"}`)),
      h("span", { class: "amt" }, `${coinNum(x.value)} ${tk}`))) : [h("div", { class: "x-empty" }, "No coins yet.")]));
  }).catch((e) => coinsBox.replaceChildren(h("div", { class: "x-empty bad" }, e.message)));
  return h("div", { class: "wl-panel" },
    h("h2", {}, v.name || "Vault"),
    addrBox,
    h("div", { class: "row" },
      h("button", { onclick: async () => { try { showAddr(await api("multisig.address", { id: v.id })); } catch (e) { msg($("ms-msg"), e.message, "bad"); } } }, "New receive address"),
      h("button", { onclick: () => { if (confirm(`Forget the vault "${v.name || v.id}" here? Its coins stay where they are; you need its descriptor to see or spend them again.`)) api("multisig.remove", { id: v.id }).then(() => { MS.open = null; drawVaults(ch); }).catch((e) => msg($("ms-msg"), e.message, "bad")); } }, "Remove from this arcade")),
    h("h3", {}, "Coins"), coinsBox,
    h("h3", {}, "Co-signers"),
    h("div", { class: "wl-rows" }, ...v.signers.map((s, i) => h("div", { class: "wl-row" }, h("span", { class: "ic" }, String(i + 1)),
      h("div", { class: "mid" }, h("div", {}, s.where === "here" ? "This wallet" : "Another co-signer"), h("div", { class: "s", translate: "no" }, s.key.slice(0, 32) + "…"))))),
    h("h3", {}, "Descriptor (backup)"),
    h("p", { class: "wl-hint" }, "Give this to every co-signer and keep it with your 12 words: without it, two phrases are not enough to find or spend the vault's coins. It shows every address of the vault, so keep it private."),
    h("textarea", { rows: 4, readonly: true, spellcheck: "false" }, v.descriptor),
    h("div", { class: "row" }, h("button", { class: "small", onclick: () => navigator.clipboard.writeText(v.descriptor) }, "Copy descriptor")),
    qrImg(v.descriptor));
}

function backupChoice() {
  return h("div", {},
    h("p", { class: "wl-hint" }, "Backup of the descriptor:"),
    h("label", {}, h("input", { type: "radio", name: "ms-backup", value: "self", checked: true }), " I keep it myself (text or QR)"),
    h("label", { class: "wl-hint" }, h("input", { type: "radio", name: "ms-backup", value: "mesh", disabled: true }), " Also keep an encrypted copy on the mesh, for co-signers only (comes with mesh co-signing)"));
}

function newVaultForm(ch) {
  const segwit = ch.key.startsWith("ltc");
  const keyBox = h("div", {}, h("p", { class: "wl-hint" }, "Making this arcade's key…"));
  const wrap = h("select", { id: "ms-wrap" }, ...(segwit ? [["wsh", "SegWit (wsh)"], ["sh-wsh", "wrapped SegWit (sh-wsh)"], ["sh", "legacy (sh)"]] : [["sh", "legacy (sh)"]]).map(([v, t]) => h("option", { value: v }, t)));
  const showKey = async () => {
    try {
      const k = await api("multisig.key", { chain: ch.key, wrap: wrap.value });
      keyBox.replaceChildren(h("p", { class: "wl-hint" }, `Your key for vault ${k.vault}: give it to the other co-signers if one of them makes the vault instead.`),
        h("textarea", { rows: 3, readonly: true, spellcheck: "false" }, k.key),
        h("div", { class: "row" }, h("button", { class: "small", onclick: () => navigator.clipboard.writeText(k.key) }, "Copy my key")));
    } catch (e) { keyBox.replaceChildren(h("p", { class: "msg bad" }, e.message)); }
  };
  wrap.onchange = showKey;
  showKey();
  const others = h("textarea", { id: "ms-keys", rows: 4, placeholder: "the two other co-signers' keys, one per line ([…]tpub…/<0;1>/*)", spellcheck: "false", autocomplete: "off" });
  const name = h("input", { id: "ms-name", placeholder: "name (optional)", maxlength: 60, autocomplete: "off" });
  return h("div", { class: "wl-panel" },
    h("h2", {}, "New 2-of-3 vault"),
    h("div", { class: "row" }, h("label", { class: "wl-hint" }, "Address type ", wrap), name),
    keyBox,
    others,
    backupChoice(),
    h("div", { class: "row" },
      h("button", { class: "primary", onclick: async () => {
        try {
          const r = await api("multisig.create", { chain: ch.key, cosigners: others.value, name: name.value, wrap: wrap.value, backup: document.querySelector("input[name=ms-backup]:checked").value });
          MS.form = null; MS.open = r.vault.id; await drawVaults(ch);
          msg($("ms-msg"), `Vault made. First address ${r.vault.first_address}: check that every co-signer sees the same one. ${r.note || ""}`, "ok");
        } catch (e) { msg($("ms-msg"), e.message, "bad"); }
      } }, "Make vault"),
      h("button", { onclick: () => { MS.form = null; drawVaults(ch); } }, "Cancel")));
}

function addVaultForm(ch) {
  const d = h("textarea", { rows: 4, placeholder: "sh(sortedmulti(2,…))#checksum", spellcheck: "false", autocomplete: "off" });
  const name = h("input", { placeholder: "name (optional)", maxlength: 60, autocomplete: "off" });
  return h("div", { class: "wl-panel" },
    h("h2", {}, "Add a vault from its descriptor"),
    h("p", { class: "wl-hint" }, "Watch-only unless this wallet's 12 words hold one of its keys."),
    d, h("div", { class: "row" }, name), backupChoice(),
    h("div", { class: "row" },
      h("button", { class: "primary", onclick: async () => {
        try {
          const r = await api("multisig.import", { chain: ch.key, descriptor: d.value, name: name.value, backup: document.querySelector("input[name=ms-backup]:checked").value });
          MS.form = null; MS.open = r.vault.id; await drawVaults(ch);
          msg($("ms-msg"), r.added ? `Added. First address ${r.vault.first_address}: check it matches the other co-signers'.` : "That vault was already here.", "ok");
        } catch (e) { msg($("ms-msg"), e.message, "bad"); }
      } }, "Add vault"),
      h("button", { onclick: () => { MS.form = null; drawVaults(ch); } }, "Cancel")));
}
