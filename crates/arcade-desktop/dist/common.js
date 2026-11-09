// Shared by the main window (index.html) and app windows (appwindow.html); loaded first.

// What people call each coin (the keys underneath stay as the mesh knows them).
const TICKERS = { peptest: "PEPtest", pep: "PEP", doge: "DOGE", dgb: "DGB", dgbtest: "DGBtest", kot: "KOT", flop: "FLOP", ltc: "LTC", ltctest: "LTCtest", btc: "BTC", wjk: "WJK" };
// A chain's colour, behind its letter when it has no icon.
const COLORS = { peptest: "#2e8b44", pep: "#3f9c55", doge: "#c2a633", dgb: "#0066cc", dgbtest: "#3388dd", kot: "#7a4cc2", flop: "#d0602a", ltc: "#345d9d", ltctest: "#5b7fb8", btc: "#f7931a" };

// The home chain (chains.list, "home"): fees, tips and posts are paid in its coin. Until the
// list is in, Pepecoin testnet (the default home).
const HOME = { key: "peptest", name: "Pepecoin testnet", known: false };
const homeTicker = () => TICKERS[HOME.key] || HOME.key;
const homeCoins = (sats) => (sats / 1e8).toLocaleString(undefined, { maximumFractionDigits: 8 }) + " " + homeTicker();
async function loadHome() {
  if (HOME.known) return HOME;
  try {
    const r = await window.__TAURI__.core.invoke("api", { req: { cmd: "chains.list" } });
    const c = r.ok && (r.result || []).find((x) => x.home);
    if (c) {
      Object.assign(HOME, { key: c.key, name: c.name, known: true });
      document.querySelectorAll(".home-ticker").forEach((e) => { e.textContent = homeTicker(); });
      window.dispatchEvent(new CustomEvent("arcade:home", { detail: HOME }));
    }
  } catch {}
  return HOME;
}
loadHome();
