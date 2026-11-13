//! `arcade`: command line for a running arcaded (whiteboard step 12).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

const HELP: &str = "arcade — Arcade V1.0

  arcade setup                   first-run setup (mesh, chain, wallet, handle)
  arcade status                  sync, tag index, mesh links
  arcade peers                   mesh links
  arcade wallet create           new wallet; shows the 12-word recovery phrase ONCE
  arcade wallet restore          restore from a recovery phrase (typed at a prompt, not shown)
  arcade wallet phrase           show the recovery phrase
  arcade wallet keys             tag keys, addresses, balances
  arcade tag check <tag>         is @tag free?
  arcade tag claim <tag>         claim @tag (asks the mesh for test coins if needed)
  arcade tag list                your claims and their status
  arcade faucet                  ask the mesh for test coins
  arcade events                  recent activity
  arcade mempool                 unconfirmed transactions and pending tag claims
  arcade chain list                         chains you can install, and their state
  arcade chain add <key> [--mode full|chain] [--with ledger,profiles,ord,names,maps,tokens]
                     every index is opt-in; names, maps and tokens ride on ord
  arcade chain status <key>                 staged progress (headers, blocks, coins, indexes)
  arcade chain offers                       which arcades on the mesh offer which chains
  arcade chain sizes                        measure this arcade's chains now; sizes other arcades report
  arcade chain copy status [key]             Core-data copies (cb) offered, fetches in progress, trusted/verified coin sets
  arcade chain copy start <key> [coins|all]  bootstrap from the best copy (coins: coin set only, the default)
  arcade chain copy cancel|verify <key>      stop a fetch (sync it myself) | re-check a copied coin set now
  arcade chain refill <key> [from to [sources]]  fetch back lost block bodies (2+ independent sources must agree)
  arcade chain refill status [key] | cancel <key>   a refill's progress | stop it
  arcade chain index copy status [key]       index copies old arcades offer, fetches, votes, copied indexes
  arcade chain index copy start <key> ledger|ord [--from NODE]   take a copy now (2+ other arcades must agree)
  arcade chain index copy cancel <key> ledger|ord                stop fetching it
  arcade chain pause|resume|remove <key>
  arcade chain index add|remove <key> <index>
  arcade ledger [height]         ledger fingerprints vs. other arcades; a block's digest rows
  arcade profile <@tag|address>  a handle's address, balance, published profile and linked wallets
  arcade ord [id|number] [--chain k]       an inscription (or the ord index's state)
  arcade ord chunks [from] [n] [--chain k] dx chunk hashes, as other arcades compare
  arcade name <name> [--chain k]           who holds a name (spec, json, doge2023 readings)
  arcade map [<n>.<namespace>] [--chain k] a block map claim (or claims per namespace)
  arcade token [tick] [--chain k]          a PRC-20/DRC-20 token
  arcade tokens <address> [--chain k]      token balances of an address
  arcade mine start [threads]    mine Pepecoin testnet on this PC (pays your wallet)
  arcade mine stop | status

  --home DIR   arcade data folder (default: $ARCADE_HOME or ~/.local/share/arcade)";

fn home(args: &[String]) -> std::path::PathBuf {
    if let Some(i) = args.iter().position(|a| a == "--home") {
        return args[i + 1].clone().into();
    }
    if let Some(h) = std::env::var_os("ARCADE_HOME") {
        return h.into();
    }
    // The installed service lives in /var/lib/arcade.
    let system = std::path::Path::new("/var/lib/arcade");
    if system.join("arcaded.sock").exists() || system.join("arcade.mdbx").exists() {
        return system.to_path_buf();
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("arcade")
}

fn call(sock: &std::path::Path, req: Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("arcaded is not running ({}: {e})", sock.display()))?;
    writeln!(s, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&line).map_err(|_| "bad reply".to_string())?;
    if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err(v["error"].as_str().unwrap_or("unknown error").to_string())
    }
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    s.trim().to_string()
}

fn step(n: u32, title: &str) {
    println!("\n[{n}/12] {title}");
}

/// Wait until `done(status)` holds, redrawing one progress line.
fn wait_status(sock: &std::path::Path, what: &str, done: impl Fn(&Value) -> bool, line: impl Fn(&Value) -> String) -> Value {
    loop {
        if let Ok(st) = call(sock, json!({"cmd": "status"})) {
            if done(&st) {
                println!("\r      {:<70}", line(&st));
                return st;
            }
            print!("\r      {:<70}", line(&st));
            std::io::stdout().flush().ok();
        } else {
            print!("\r      waiting for the arcade service ({what})…{:<20}", "");
            std::io::stdout().flush().ok();
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// First-run setup: the whiteboard steps, in order, on the command line.
fn setup(sock: &std::path::Path) {
    println!("Arcade V1.0 setup — Pepecoin testnet");

    step(1, "Install arcade");
    match call(sock, json!({"cmd": "status"})) {
        Ok(_) => println!("      the arcade service is running"),
        Err(e) => {
            println!("      {e}\n      start it with:  sudo systemctl enable --now arcaded   then run  arcade setup  again");
            std::process::exit(2);
        }
    }

    step(2, "Connect to the mesh");
    wait_status(sock, "mesh", |s| s["mesh"]["links"].as_u64().unwrap_or(0) > 0, |s| format!("{} arcade(s) linked", s["mesh"]["links"]));

    step(3, "Pepecoin testnet");
    let st = call(sock, json!({"cmd": "status"})).unwrap_or_default();
    println!("      chain: {}", st["chain"].as_str().unwrap_or("?"));

    step(4, "Get the Pepecoin testnet blockchain (mesh first, Pepecoin P2P as fallback)");
    wait_status(sock, "sync", |s| s["sync"].as_str().unwrap_or("").starts_with("synced"), |s| s["sync"].as_str().unwrap_or("").to_string());

    step(5, "Build the tag index");
    let st = wait_status(
        sock,
        "index",
        |s| s["tip"]["height"].as_u64().map(|t| s["tags"]["indexed_to"].as_u64().unwrap_or(0) >= t).unwrap_or(false),
        |s| format!("indexed to {} ({} tags)", s["tags"]["indexed_to"], s["tags"]["count"]),
    );
    let ours = st["tags"]["names_digest"].as_str().unwrap_or("");
    let tips = st["mesh"]["tips"].as_array().cloned().unwrap_or_default();
    let told: Vec<_> = tips.iter().filter(|t| t["names_digest"].is_string()).collect();
    let same = told.iter().filter(|t| t["names_digest"] == ours).count();
    println!("      matches {same} of {} arcades that reported", told.len());

    step(6, "Your wallet");
    if st["wallet"] == true {
        println!("      a wallet already exists on this arcade");
    } else {
        loop {
            match ask("      (n)ew wallet or (r)estore from 12 words? ").as_str() {
                "n" | "N" => {
                    let r = call(sock, json!({"cmd": "wallet.create"})).unwrap_or_else(|e| { eprintln!("      {e}"); std::process::exit(1) });
                    println!("\n      Your recovery phrase — write it down now, it is shown only once:\n\n        {}\n", r["phrase"].as_str().unwrap_or(""));
                    while ask("      Type  yes  when you have written it down: ") != "yes" {}
                    print!("\x1b[2J\x1b[H"); // clear the screen so the phrase does not stay visible
                    println!("Arcade V1.0 setup — continuing");
                    break;
                }
                "r" | "R" => {
                    let words = read_hidden("      your 12 words (not shown): ").unwrap_or_else(|_| ask("      your 12 words: "));
                    match call(sock, json!({"cmd": "wallet.create", "phrase": words})) {
                        Ok(_) => { println!("      wallet restored"); break; }
                        Err(e) => println!("      {e}"),
                    }
                }
                _ => {}
            }
        }
    }

    step(7, "Ask for a handle");
    let keys = call(sock, json!({"cmd": "wallet.keys"})).unwrap_or_default();
    if let Some(t) = keys.as_array().and_then(|k| k.iter().find_map(|k| k["tag"].as_str().map(String::from))) {
        println!("      you already hold @{t}");
    } else {
        let tag = loop {
            let t = ask("      which @tag would you like? ");
            step(8, "Check the handle");
            match call(sock, json!({"cmd": "tag.check", "tag": t})) {
                Ok(r) if r["available"] == true => { println!("      @{} is free", r["tag"].as_str().unwrap_or("")); break t; }
                Ok(r) => println!("      @{} is not available: {}", r["tag"].as_str().unwrap_or(""), r["reason"].as_str().unwrap_or("")),
                Err(e) => println!("      {e}"),
            }
        };
        step(9, "Get test coins and claim the handle");
        match call(sock, json!({"cmd": "tag.claim", "tag": tag})) {
            Err(e) => println!("      {e}"),
            Ok(c) => {
                let id = c["id"].as_str().unwrap_or("").to_string();
                let mut last = String::new();
                loop {
                    let list = call(sock, json!({"cmd": "tag.list"})).unwrap_or_default();
                    if let Some(c) = list.as_array().and_then(|l| l.iter().find(|c| c["id"] == id.as_str())) {
                        let line = format!("{}: {}", c["status"].as_str().unwrap_or(""), c["note"].as_str().unwrap_or(""));
                        if line != last { println!("      {line}"); last = line; }
                        if c["status"] == "confirmed" || c["status"] == "failed" { break; }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            }
        }
    }

    step(10, "Start at boot");
    let enabled = std::process::Command::new("systemctl").args(["is-enabled", "arcaded"]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled").unwrap_or(false);
    println!("      {}", if enabled { "arcaded starts at boot" } else { "not enabled: sudo systemctl enable arcaded" });

    step(11, "Share bootstraps");
    println!("      your arcade serves its chain to other arcades on the mesh (always on)");

    step(12, "Command line");
    println!("      run  arcade  for every command; the desktop app shows the same arcade.\n\nSetup complete.");
}

/// The phrase for `wallet restore`: typed at a prompt that doesn't show it, or (with a warning:
/// it stays in the shell's history and shows in the process list) given on the command line.
fn restore_phrase(words: &[&str], typed: impl FnOnce() -> std::io::Result<String>) -> Result<String, String> {
    if !words.is_empty() {
        eprintln!("warning: words on the command line stay in your shell's history and show in the process list; next time run `arcade wallet restore` and type them at the prompt");
        return Ok(words.join(" "));
    }
    let phrase = typed().map_err(|e| format!("could not read the words: {e}"))?.split_whitespace().collect::<Vec<_>>().join(" ");
    if phrase.is_empty() {
        return Err("no words given".into());
    }
    Ok(phrase)
}

fn stty(tty: &std::fs::File, arg: &str) -> std::io::Result<()> {
    let ok = std::process::Command::new("stty").arg(arg).stdin(tty.try_clone()?).status()?.success();
    if ok { Ok(()) } else { Err(std::io::Error::other("stty failed")) }
}

/// A line typed at the terminal without showing it; from standard input when there is no
/// terminal (`arcade wallet restore < file`).
fn read_hidden(prompt: &str) -> std::io::Result<String> {
    let mut line = String::new();
    let Ok(mut tty) = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        std::io::stdin().read_line(&mut line)?;
        return Ok(line.trim().to_string());
    };
    write!(tty, "{prompt}")?;
    tty.flush()?;
    stty(&tty, "-echo")?;
    let got = BufReader::new(tty.try_clone()?).read_line(&mut line);
    let _ = stty(&tty, "echo");
    let _ = writeln!(tty);
    got.map(|_| line.trim().to_string())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let sock = home(&args).join("arcaded.sock");
    if let Some(i) = args.iter().position(|a| a == "--home") {
        args.drain(i..i + 2);
    }
    let mut chain: Option<String> = None;
    if let Some(i) = args.iter().position(|a| a == "--chain") {
        chain = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    if a.as_slice() == ["setup"] {
        setup(&sock);
        return;
    }
    let req = match a.as_slice() {
        ["status"] => json!({"cmd": "status"}),
        ["peers"] => json!({"cmd": "peers"}),
        ["wallet", "create"] => json!({"cmd": "wallet.create"}),
        ["wallet", "restore", words @ ..] => match restore_phrase(words, || read_hidden("Your recovery phrase (not shown as you type): ")) {
            Ok(phrase) => json!({"cmd": "wallet.create", "phrase": phrase}),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },
        ["wallet", "phrase"] => json!({"cmd": "wallet.phrase"}),
        ["wallet", "keys"] => json!({"cmd": "wallet.keys"}),
        ["tag", "check", t] => json!({"cmd": "tag.check", "tag": t}),
        ["tag", "claim", t] => json!({"cmd": "tag.claim", "tag": t}),
        ["tag", "list"] => json!({"cmd": "tag.list"}),
        ["faucet"] => json!({"cmd": "faucet.ask"}),
        ["events"] => json!({"cmd": "events"}),
        ["mempool"] => json!({"cmd": "mempool"}),
        ["profile", who] => json!({"cmd": "profile", "who": who}),
        ["ord"] => json!({"cmd": "ord"}),
        ["ord", "chunks"] => json!({"cmd": "ord.chunks"}),
        ["ord", "chunks", f] => json!({"cmd": "ord.chunks", "from": f.parse::<u64>().unwrap_or(0)}),
        ["ord", "chunks", f, n] => json!({"cmd": "ord.chunks", "from": f.parse::<u64>().unwrap_or(0), "n": n.parse::<u64>().unwrap_or(10)}),
        ["ord", id] => json!({"cmd": "ord", "id": id}),
        ["name", n] => json!({"cmd": "name", "name": n}),
        ["map"] => json!({"cmd": "map"}),
        ["map", m] => json!({"cmd": "map", "claim": m}),
        ["token"] => json!({"cmd": "token"}),
        ["token", t] => json!({"cmd": "token", "tick": t}),
        ["tokens", a] => json!({"cmd": "tokens", "owner": a}),
        ["chain", "list"] | ["chains"] => json!({"cmd": "chains.list"}),
        ["chain", "sizes"] => json!({"cmd": "chains.sizes"}),
        ["chain", "offers"] => json!({"cmd": "chains.offers"}),
        ["chain", "status", k] => json!({"cmd": "chains.status", "key": k}),
        ["chain", "pause", k] => json!({"cmd": "chains.pause", "key": k}),
        ["chain", "resume", k] => json!({"cmd": "chains.resume", "key": k}),
        ["chain", "remove", k] => json!({"cmd": "chains.remove", "key": k}),
        ["chain", "copy", "status"] => json!({"cmd": "chains.copy_status"}),
        ["chain", "copy", "status", k] => json!({"cmd": "chains.copy_status", "key": k}),
        ["chain", "copy", "start", k] => json!({"cmd": "chains.copy_start", "key": k}),
        ["chain", "copy", "start", k, w] => json!({"cmd": "chains.copy_start", "key": k, "what": w}),
        ["chain", "copy", "cancel", k] => json!({"cmd": "chains.copy_cancel", "key": k}),
        ["chain", "check-rules", "status"] => json!({"cmd": "chains.check_rules_status"}),
        ["chain", "check-rules", "status", k] => json!({"cmd": "chains.check_rules_status", "key": k}),
        ["chain", "check-rules", "cancel", k] => json!({"cmd": "chains.check_rules_cancel", "key": k}),
        ["chain", "check-rules", k] => json!({"cmd": "chains.check_rules", "key": k}),
        ["chain", "check-rules", k, from, to] => json!({"cmd": "chains.check_rules", "key": k, "from": from.parse::<u64>().ok(), "to": to.parse::<u64>().ok()}),
        ["chain", "check-rules", k, from, to, n] => json!({"cmd": "chains.check_rules", "key": k, "from": from.parse::<u64>().ok(), "to": to.parse::<u64>().ok(), "threads": n.parse::<u64>().ok()}),
        ["chain", "refill", "status"] => json!({"cmd": "chains.refill_status"}),
        ["chain", "refill", "status", k] => json!({"cmd": "chains.refill_status", "key": k}),
        ["chain", "refill", "cancel", k] => json!({"cmd": "chains.refill_cancel", "key": k}),
        ["chain", "refill", k] => json!({"cmd": "chains.refill", "key": k}),
        ["chain", "refill", k, from, to] => json!({"cmd": "chains.refill", "key": k, "from": from.parse::<u64>().ok(), "to": to.parse::<u64>().ok()}),
        ["chain", "refill", k, from, to, n] => json!({"cmd": "chains.refill", "key": k, "from": from.parse::<u64>().ok(), "to": to.parse::<u64>().ok(), "sources": n.parse::<u64>().ok()}),
        ["chain", "copy", "verify", k] => json!({"cmd": "chains.copy_verify", "key": k}),
        ["chain", "share", "offers", k] => json!({"cmd": "share.offers", "key": k}),
        ["chain", "share", "fetch", k, f] => json!({"cmd": "share.fetch", "key": k, "fam": f}),
        ["chain", "share", "fetch", k, f, id] => json!({"cmd": "share.fetch", "key": k, "fam": f, "id": id}),
        ["chain", "share", "cancel", k, f] => json!({"cmd": "share.cancel", "key": k, "fam": f}),
        ["chain", "share", "install", k] => json!({"cmd": "share.install", "key": k}),
        ["chain", "share", "install", k, "cancel"] => json!({"cmd": "share.install_cancel", "key": k}),
        ["chain", "share", "serve", k, "on"] => json!({"cmd": "share.serve", "key": k, "on": true}),
        ["chain", "share", "serve", k, "off"] => json!({"cmd": "share.serve", "key": k, "on": false}),
        ["chain", "share", "held"] => json!({"cmd": "share.held"}),
        ["chain", "share", "make", k, f] => json!({"cmd": "share.make", "key": k, "fam": f}),
        ["chain", "index", "copy", "status"] => json!({"cmd": "chains.index_copy_status"}),
        ["chain", "index", "copy", "status", k] => json!({"cmd": "chains.index_copy_status", "key": k}),
        ["chain", "index", "copy", "start", k, ix] => json!({"cmd": "chains.index_copy_start", "key": k, "index": ix}),
        ["chain", "index", "copy", "start", k, ix, "--from", node] => json!({"cmd": "chains.index_copy_start", "key": k, "index": ix, "from": node}),
        ["chain", "index", "copy", "cancel", k, ix] => json!({"cmd": "chains.index_copy_cancel", "key": k, "index": ix}),
        ["chain", "index", "add", k, ix] => json!({"cmd": "chains.index_add", "key": k, "index": ix}),
        ["chain", "index", "remove", k, ix] => json!({"cmd": "chains.index_remove", "key": k, "index": ix}),
        ["chain", "add", k, rest @ ..] => {
            let mut mode = "full".to_string();
            let mut with: Vec<String> = vec![];
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--mode" if i + 1 < rest.len() => { mode = rest[i + 1].to_string(); i += 1; }
                    "--with" if i + 1 < rest.len() => { with = rest[i + 1].split(',').map(String::from).collect(); i += 1; }
                    _ => {}
                }
                i += 1;
            }
            json!({"cmd": "chains.add", "key": k, "mode": mode, "with": with})
        }
        ["ledger"] => json!({"cmd": "ledger"}),
        ["ledger", h] => json!({"cmd": "ledger", "height": h}),
        ["mine", "start"] => json!({"cmd": "mine.start"}),
        ["mine", "start", n] => json!({"cmd": "mine.start", "threads": n}),
        ["mine", "stop"] => json!({"cmd": "mine.stop"}),
        ["mine"] | ["mine", "status"] => json!({"cmd": "mine.status"}),
        _ => {
            println!("{HELP}");
            return;
        }
    };
    let mut req = req;
    if let Some(k) = &chain {
        req["chain"] = json!(k);
    }
    let mut s = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("arcaded is not running ({}: {e})", sock.display());
            std::process::exit(2);
        }
    };
    writeln!(s, "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap_or(json!({"ok": false, "error": "bad reply"}));
    if v["ok"] == true {
        if a.first() == Some(&"wallet") && a.get(1) == Some(&"create") {
            if let Some(p) = v["result"]["phrase"].as_str() {
                println!("Your recovery phrase (write it down, it is shown only now):\n\n  {p}\n");
                return;
            }
        }
        if (a.first() == Some(&"chain") && matches!(a.get(1).copied(), Some("status") | Some("add") | Some("pause") | Some("resume"))) && v["result"]["stages"].is_array() {
            let r = &v["result"];
            println!("{} — {} · {} · {}", r["name"].as_str().unwrap_or(""), r["mode"].as_str().unwrap_or(""), r["state"].as_str().unwrap_or(""), r["indexes"].as_array().map(|x| x.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default());
            for st in r["stages"].as_array().unwrap() {
                let p = st["pct"].as_f64().unwrap_or(0.0);
                let bar: String = "█".repeat((p / 5.0).round() as usize);
                let per = if st["unit"] == "rows" { "rows/s" } else { "blk/s" };
                let rate = match st["rate"].as_f64() {
                    Some(r) if r >= 10.0 => format!("{r:.0} {per}"),
                    Some(r) => format!("{r:.1} {per}"),
                    None if st["info"] == true => String::new(),
                    None => "measuring".into(),
                };
                println!("  {:<34} {:<20} {:>5.1}% {:>13}  {}", st["name"].as_str().unwrap_or(""), format!("{bar:░<20}"), p, rate, st["detail"].as_str().unwrap_or(""));
            }
            println!("  overall {:.1}%", r["overall"].as_f64().unwrap_or(0.0));
            return;
        }
        if a.first() == Some(&"chain") && a.get(1) == Some(&"list") || a.first() == Some(&"chains") {
            println!("{:<9} {:<20} {:<14} {:<6} INDEXES", "KEY", "CHAIN", "STATE", "MODE");
            for c in v["result"].as_array().unwrap() {
                let st = if c["installed"] == true { if c["home"] == true { "home".to_string() } else { format!("{:.0}% {}", c["overall"].as_f64().unwrap_or(0.0), c["state"].as_str().unwrap_or("")) } } else { "available".into() };
                println!("{:<9} {:<20} {:<14} {:<6} {}", c["key"].as_str().unwrap_or(""), c["name"].as_str().unwrap_or(""), st.chars().take(14).collect::<String>(), c["mode"].as_str().unwrap_or(""), c["indexes"].as_array().map(|x| x.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default());
            }
            return;
        }
        println!("{}", serde_json::to_string_pretty(&v["result"]).unwrap());
    } else {
        eprintln!("error: {}", v["error"].as_str().unwrap_or("unknown"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_asks_for_the_words_when_none_are_given() {
        let mut asked = false;
        let p = restore_phrase(&[], || {
            asked = true;
            Ok("  one two\tthree \n".into())
        });
        assert_eq!(p.unwrap(), "one two three");
        assert!(asked);
        assert!(restore_phrase(&[], || Ok("\n".into())).is_err(), "nothing typed");
        assert!(restore_phrase(&[], || Err(std::io::Error::other("no terminal"))).is_err());
    }

    #[test]
    fn restore_still_takes_words_on_the_command_line() {
        let p = restore_phrase(&["one", "two", "three"], || panic!("no prompt when the words are given"));
        assert_eq!(p.unwrap(), "one two three");
    }
}
