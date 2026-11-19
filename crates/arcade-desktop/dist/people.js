// People tab: A:// on the ARCADE chain (Martin 2026-11-15): your profile card, who you follow and who follows you,
// and an address book only you can read (each entry sealed to your own key). Until a profile is published on ARCADE
// the one published on the home chain is shown, with a note to republish it once the wallet has test ARCADE.
(() => {
  const ARC = (sats) => (sats / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 }) + " ARCADE";
  const CHAINS = [["pep", "Pepecoin"], ["doge", "Dogecoin"], ["ltc", "Litecoin"], ["dgb", "DigiByte"], ["btc", "Bitcoin"]];
  const NAMES = { pep: "Pepecoin", peptest: "Pepecoin testnet", doge: "Dogecoin", ltc: "Litecoin", ltctest: "Litecoin testnet", dgb: "DigiByte", dgbtest: "DigiByte testnet", btc: "Bitcoin", flop: "Flopcoin", wjk: "Wojakcoin", kot: "Kotia", jkc: "Junkcoin", shic: "Shibacoin", lky: "Luckycoin" };
  // Where a name was published: ARCADE, another chain's arcade ledger, or DogeFeed on Dogecoin.
  const homePill = (home) => !home ? "" : h("span", { class: "pill pp-home" + (home.key === "arctest" || home.key === "arc" ? " ok" : ""), title: `Published ${home.via === "DogeFeed" ? "as a DogeFeed name" : home.via === "A://" ? "as an A:// profile" : home.via === "profile" ? "as a profile" : "as an @tag"} on ${home.chain}` },
    home.via === "DogeFeed" ? "DogeFeed · Dogecoin" : home.chain);
  const label = (k) => { const [c, n] = k.split("-"); return (NAMES[c] || c) + (n ? " " + n : ""); };
  const note = (t, bad) => say("pp-msg", t, bad);
  let view = "profile", me = null, picture = null, dirty = false, contacts = [];

  function show(v) {
    view = v;
    document.querySelectorAll("#pp-sub button").forEach((b) => b.classList.toggle("on", b.dataset.v === v));
    ["profile", "follows", "contacts"].forEach((x) => { $("pp-" + x).hidden = x !== v; });
    draw();
  }
  document.querySelectorAll("#pp-sub button").forEach((b) => { b.onclick = () => show(b.dataset.v); });

  // A picture blob, shown from its bytes (any arcade fetches it from the chain).
  const pics = {};
  function avatar(p, small) {
    const name = (p && (p.profile?.n || p.tag)) || "?";
    const el = h("div", { class: "pp-av" + (small ? " sm" : "") }, name.replace(/^@/, "").slice(0, 1).toUpperCase());
    const id = p?.profile?.p;
    if (id) {
      const put = (src) => { const img = h("img", { class: "pp-av" + (small ? " sm" : ""), src, alt: "" }); el.replaceWith(img); };
      if (pics[id]) setTimeout(() => put(pics[id]));
      else api("arc.blob", { id }).then((b) => {
        const bytes = b.hex.match(/../g).map((x) => parseInt(x, 16));
        pics[id] = URL.createObjectURL(new Blob([new Uint8Array(bytes)]));
        put(pics[id]);
      }).catch(() => {});
    }
    return el;
  }

  function cardView(p) {
    const pr = p.profile || {};
    return [
      h("div", { class: "pp-prof" }, avatar(p), h("div", {}, h("div", { class: "name" }, pr.n || (p.tag ? "@" + p.tag : "No name yet")), h("div", { class: "pp-tag" }, p.tag ? "@" + p.tag : p.address))),
      h("div", { class: "row", style: "margin:8px 0 0" }, h("span", { class: "s" }, "Home chain"), homePill(p.from_home ? { chain: p.from_home.chain, key: "", via: "profile" } : { chain: "ARCADE", key: "arctest", via: "A://" })),
      pr.b ? h("p", { class: "s" }, pr.b) : "",
      pr.u ? h("p", { class: "s" }, pr.u) : "",
      h("div", { class: "pp-stats" },
        h("div", {}, h("b", {}, String(p.followers ?? 0)), h("span", { class: "s" }, "followers")),
        h("div", {}, h("b", {}, String(p.following ?? 0)), h("span", { class: "s" }, "following"))),
      h("div", { class: "pp-addrs", style: "margin-top:12px" },
        h("div", { class: "pp-addr" }, h("span", { class: "k" }, "ARCADE"), h("span", { class: "mono" }, p.address)),
        // Proven on the chain (A:// wallet proofs: each address signed with its own key), then any the profile only
        // states.
        ...(p.wallets || []).map((w) => h("div", { class: "pp-addr", title: `Signed by this address's own key (block ${w.height})` },
          h("span", { class: "k" }, w.coin), h("span", { class: "mono" }, w.address), h("span", { class: "pp-ok" }, "✓ verified"))),
        ...Object.entries(pr.a || {}).filter(([, v]) => !(p.wallets || []).some((w) => w.address === v)).map(([k, v]) =>
          h("div", { class: "pp-addr", title: "Stated on the profile, not signed by its key" },
            h("span", { class: "k" }, k), h("span", { class: "mono" }, v), h("span", { class: "pp-no" }, "not verified")))),
      p.from_home ? h("p", { class: "s" }, `Shown from ${p.from_home.chain} until it is published on ARCADE.`) : (p.height != null ? h("p", { class: "s" }, `Published at block ${p.height} · version ${p.versions}.`) : ""),
    ];
  }

  // The profile's rows: every address the wallet has (its own on each coin here, imported wallets), kept editable.
  function profileAddrInputs(holder, values) {
    holder.replaceChildren(...Object.entries(values).map(([k, v]) => {
      const i = h("input", { "data-chain": k, value: v, placeholder: `${label(k)} address` });
      i.oninput = () => { dirty = true; cost(); };
      return h("div", { class: "pp-addr" }, h("span", { class: "k" }, label(k)), i);
    }));
    if (!Object.keys(values).length) holder.replaceChildren(h("span", { class: "s" }, "No addresses in this wallet yet."));
  }
  function addrInputs(holder, values) {
    holder.replaceChildren(...CHAINS.map(([k, name]) => {
      const i = h("input", { "data-chain": k, value: values?.[k] || "", placeholder: `${name} address` });
      i.oninput = () => { dirty = true; cost(); };
      return h("div", { class: "pp-addr" }, h("span", { class: "k" }, name), i);
    }));
  }
  const addrValues = (holder) => Object.fromEntries([...holder.querySelectorAll("input")].map((i) => [i.dataset.chain, i.value.trim()]).filter(([, v]) => v));

  function formProfile() {
    return { name: $("pp-name").value.trim(), bio: $("pp-bio").value.trim(), url: $("pp-url").value.trim(), picture: picture || undefined, addresses: addrValues($("pp-addrs")) };
  }
  function cost() {
    const f = formProfile();
    const body = JSON.stringify({ n: f.name || undefined, b: f.bio || undefined, u: f.url || undefined, p: f.picture, a: Object.keys(f.addresses).length ? f.addresses : undefined });
    $("pp-cost").textContent = `One record, about ${body.length} bytes · fee about ${ARC(Math.max(45000, Math.ceil((body.length + 250) / 1000) * 100000))}`;
  }

  async function drawProfile() {
    me = await api("a.profile");
    $("pp-card").replaceChildren(...cardView(me));
    if (!dirty) {
      const pr = me.profile || {};
      $("pp-name").value = pr.n || ""; $("pp-bio").value = pr.b || ""; $("pp-url").value = pr.u || "";
      picture = pr.p || null;
      // The wallet's addresses as they are now, plus any the published card shows that the wallet doesn't hold.
      profileAddrInputs($("pp-addrs"), { ...(pr.a || {}), ...(me.wallet_addresses || {}) });
      cost();
    }
    const ban = $("pp-republish");
    const needCoins = !me.can_publish;
    if (me.from_home || needCoins) {
      ban.hidden = false;
      ban.replaceChildren(
        me.from_home ? h("div", {}, h("b", {}, "Republish your profile on ARCADE. "), `This is the profile you published on ${me.from_home.chain}. Publish it again here so every arcade shows it on the ARCADE chain.`) : "",
        needCoins ? h("div", { class: "s" }, `Publishing needs a little test ARCADE (this wallet has ${ARC(me.spendable || 0)}). The faucet sends some to new accounts once it's running; then publish from here.`) : "");
    } else ban.hidden = true;
    $("pp-publish").disabled = needCoins;
    $("pp-publish").textContent = me.from_home ? "Republish on ARCADE" : "Publish profile";
  }

  async function drawFollows() {
    const f = await api("a.follows");
    followed = new Set(f.following.flatMap((p) => [p.tag, p.follow_key]).filter(Boolean));
    $("pp-nfollowing").textContent = String(f.following.length);
    $("pp-nfollowers").textContent = String(f.followers.length);
    const row = (p, action) => h("div", { class: "pp-person" }, avatar(p, true),
      h("div", { style: "min-width:0" }, h("div", { class: "row", style: "margin:0;gap:6px" }, h("b", {}, p.profile?.n || (p.tag ? "@" + p.tag : p.address.slice(0, 12) + "…")), homePill(p.home)), h("div", { class: "s" }, p.tag ? h("span", { class: "pp-tag" }, "@" + p.tag) : "", ` · since block ${p.since}`)), action);
    const btn = (label, fn, primary) => { const b = h("button", { class: primary ? "primary" : "" }, label); b.onclick = fn; return b; };
    $("pp-following").replaceChildren(...(f.following.length ? f.following.map((p) => row(p, btn("Unfollow", () => follow((p.follow_key || p.tag).replace(/^#/, ""), false)))) : [h("p", { class: "s" }, "Nobody yet. Follow an @tag above.")]));
    $("pp-followers").replaceChildren(...(f.followers.length ? f.followers.map((p) => row(p, p.you_follow ? h("span", { class: "pill ok" }, "You follow") : btn("Follow back", () => follow(p.tag || p.key, true), true))) : [h("p", { class: "s" }, "No followers yet.")]));
  }
  async function follow(tag, on) {
    try { const r = await api(on ? "a.follow" : "a.unfollow", { tag }); note(`${on ? "Following" : "Unfollowed"} @${r.tag} (fee ${ARC(r.fee)}); it counts once it's in a block.`); setTimeout(draw, 4000); }
    catch (e) { note(e.message, true); }
  }

  async function drawContacts() {
    contacts = (await api("a.contacts")).contacts;
    inBook = new Set(contacts.flatMap((c) => [c.addresses?.arcade, c.tag ? "@" + c.tag : null]).filter(Boolean));
    const shown = contacts;
    $("pp-cempty").hidden = contacts.length > 0;
    $("pp-crows").replaceChildren(...shown.map((c) => {
      const send = h("button", {}, "Send"); send.onclick = () => { const to = c.tag ? "@" + c.tag : Object.values(c.addresses || {})[0]; goTab("wallet"); if ($("w-to") && to) $("w-to").value = to; };
      const del = h("button", {}, "Delete");
      del.onclick = async () => {
        if (!del.dataset.sure) { del.dataset.sure = 1; del.textContent = "Delete: sure?"; return; }
        try { const r = await api("a.contact_delete", { id: c.id }); note(`Deleting ${c.name} (fee ${ARC(r.fee)}); gone once it's in a block.`); setTimeout(draw, 4000); } catch (e) { note(e.message, true); }
      };
      return h("tr", {}, h("td", {}, h("b", {}, c.name), h("div", {}, homePill(c.home))), h("td", { class: "pp-tag" }, c.tag ? "@" + c.tag : ""),
        h("td", {}, ...Object.entries(c.addresses || {}).map(([k, v]) => h("div", { class: "mono" }, `${k} ${v}`))),
        h("td", { class: "s" }, c.note || ""), h("td", {}, h("div", { class: "row", style: "margin:0" }, send, del)));
    }));
  }


  ["pp-name", "pp-bio", "pp-url"].forEach((id) => { $(id).oninput = () => { dirty = true; cost(); }; });
  $("pp-pic").onchange = (e) => {
    const f = e.target.files[0];
    if (!f) return;
    if (f.size > 200_000) { note("Pick a picture under 200 kB (it is kept on the chain).", true); return; }
    const rd = new FileReader();
    rd.onload = async () => {
      const hex = [...new Uint8Array(rd.result)].map((b) => b.toString(16).padStart(2, "0")).join("");
      try {
        const r = await api("arc.blob_publish", { hex, type: f.type || "image/png", class: 0, send: true });
        picture = r.blob; dirty = true; cost();
        $("pp-picnote").textContent = `Picture sent (fee ${ARC(r.fee)}). Publish the profile to show it.`;
      } catch (err) { note(err.message, true); }
    };
    rd.readAsArrayBuffer(f);
  };
  $("pp-publish").onclick = async () => {
    try { const r = await api("a.profile_set", formProfile()); dirty = false; note(`Profile sent (${r.bytes} bytes, fee ${ARC(r.fee)}); every arcade shows it once it's in a block.`); setTimeout(draw, 4000); }
    catch (e) { note(e.message, true); }
  };
  // One search for both: published profiles and @tags, each with its button (Martin: "a simple search and click").
  let followed = new Set(), inBook = new Set(), timer = null;
  function search(input, results, mode) {
    input.oninput = () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const q = input.value.trim();
        if (q.replace(/^@/, "").length < 2) { results.replaceChildren(); return; }
        try {
          const r = await api("a.search", { q });
          const hits = r.results.filter((p) => !(me && p.address === me.address));
          results.replaceChildren(...(hits.length ? hits.map((p) => hit(p, mode)) : [h("p", { class: "s" }, `Nobody published under "${q}" yet.`)]));
        } catch (e) { note(e.message, true); }
      }, 250);
    };
  }
  function hit(p, mode) {
    const pr = p.profile || {};
    let b;
    if (mode === "follow") {
      const target = p.tag || p.key;
      b = followed.has(p.tag) || followed.has("#" + p.key) ? h("span", { class: "pill ok" }, "Following") : h("button", { class: "primary" }, "Follow");
      if (b.tagName === "BUTTON") b.onclick = () => { b.disabled = true; follow(target, true); };
    } else {
      b = inBook.has(p.address) || (p.tag && inBook.has("@" + p.tag)) ? h("span", { class: "pill ok" }, "In your book") : h("button", { class: "primary" }, "Add");
      if (b.tagName === "BUTTON") b.onclick = async () => {
        b.disabled = true;
        try { const r = await api("a.contact_add", { key: p.key }); note(`Adding ${pr.n || "@" + p.tag} (fee ${ARC(r.fee)}); it shows once it's in a block.`); setTimeout(draw, 4000); }
        catch (e) { b.disabled = false; note(e.message, true); }
      };
    }
    return h("div", { class: "pp-hit" }, avatar(p, true),
      h("div", { style: "min-width:0" }, h("div", { class: "row", style: "margin:0;gap:6px" }, h("b", {}, pr.n || "@" + p.tag), homePill(p.home)),
        h("div", { class: "s" }, p.tag ? h("span", { class: "pp-tag" }, "@" + p.tag) : (p.other_tag ? "@" + p.other_tag : ""), pr.b ? " · " + pr.b : "", ` · ${p.followers} followers`)), b);
  }
  search($("pp-fsearch"), $("pp-fresults"), "follow");
  search($("pp-search"), $("pp-cresults"), "add");

  async function draw() {
    try {
      if (view === "profile") await drawProfile();
      else if (view === "follows") await drawFollows();
      else await drawContacts();
    } catch (e) { note(/ARCADE chain isn't running/.test(e.message) ? "Install the ARCADE testnet in Chains & Coins to use People." : e.message, true); }
  }
  document.querySelector('nav button[data-tab="people"]').addEventListener("click", () => draw());
  setInterval(() => { if (document.querySelector(".tab.on")?.id === "people") draw(); }, 30000);
})();
