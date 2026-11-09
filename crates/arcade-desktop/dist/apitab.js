// API tab: where sidecar apps and websites reach this arcade, and the website API keys.
(() => {
  const PERMS = [
    ["read", "Read private data", "your messages, groups, contacts, wallet addresses, activity"],
    ["message", "Messages", "send messages, groups, contacts and mutes"],
    ["post", "Post", "public posts, likes, replies, shares, edits, deletes"],
    ["tip", "Tip", "send tips, up to the daily limit below"],
    ["wallet", "Wallet", "claim @tags, ask the faucet, link wallets"],
    ["admin", "Run the arcade", "chains, indexes, mining"],
  ];
  const msg = (t, bad) => say("ap-msg", t, bad);
  $("ap-perms").replaceChildren(...PERMS.map(([k, t, d]) => {
    const l = document.createElement("label");
    l.innerHTML = `<input type="checkbox" value="${k}"><span><b></b><br><small class="s"></small></span>`;
    l.querySelector("b").textContent = t; l.querySelector("small").textContent = d;
    return l;
  }));
  async function draw() {
    try {
      const i = await api("rpc.info");
      $("ap-rpc").textContent = i.url; $("ap-api").textContent = i.api; $("ap-cookie").textContent = i.cookie_file;
      const keys = await api("apikey.list");
      const tb = $("ap-keys");
      if (!keys.length) { tb.innerHTML = '<tr><td colspan="5" class="s">No keys yet.</td></tr>'; return; }
      tb.replaceChildren(...keys.map((k) => {
        const tr = document.createElement("tr");
        const cells = [k.name, (k.perms || []).join(", ") || "public only", k.tip_limit ? (k.tip_limit / 1e8).toString() : "—", k.last_used ? new Date(k.last_used * 1000).toLocaleString() : "never"];
        cells.forEach((c, i) => { const td = document.createElement("td"); td.textContent = c; if (i === 2) td.className = "num"; tr.append(td); });
        const td = document.createElement("td"); const b = document.createElement("button"); b.className = "small"; b.textContent = "Revoke";
        b.onclick = async () => { if (!b.dataset.sure) { b.dataset.sure = 1; b.textContent = "Revoke: sure?"; return; } await api("apikey.revoke", { id: k.id }); msg(`Revoked "${k.name}".`); draw(); };
        td.append(b); tr.append(td); return tr;
      }));
    } catch (e) { msg(e.message, true); }
  }
  $("ap-make").onclick = async () => {
    const perms = [...document.querySelectorAll("#ap-perms input:checked")].map((i) => i.value);
    try {
      const r = await api("apikey.create", { name: $("ap-name").value, perms, tip_limit: Math.round(Number($("ap-tip").value || 0) * 1e8) });
      const out = $("ap-out");
      out.replaceChildren();
      const b = document.createElement("b"); b.textContent = "Your new key: copy it now, it isn't shown again.";
      const c = document.createElement("code"); c.textContent = r.key;
      const copy = document.createElement("button"); copy.className = "small"; copy.textContent = "Copy";
      copy.onclick = () => navigator.clipboard?.writeText(r.key).then(() => { copy.textContent = "Copied"; });
      out.append(b, document.createElement("br"), c, " ", copy);
      out.hidden = false;
      $("ap-name").value = ""; document.querySelectorAll("#ap-perms input").forEach((i) => i.checked = false);
      draw();
    } catch (e) { msg(e.message, true); }
  };
  document.querySelector('nav button[data-tab="apitab"]').addEventListener("click", () => { $("ap-out").hidden = true; draw(); });
})();
