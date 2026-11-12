//! The fast lane between arcades (a child of realtime: it shares the room state).
//!
//! A page's room joined with {fast: true} wants many small messages a second between its
//! players. The original arcade carries them browser to browser; an app window here cannot (no
//! WebRTC), so the arcade carries them itself, straight to the arcade each player plays
//! through:
//!
//! - an "fx" frame, signed by the arcade that sends it, addressed to one arcade ("to"), over the
//!   link to it when there is one, else along the room's routes (V1.0 arcades pass it on;
//!   other arcades never see a frame they do not know);
//! - "hi" / "ho" between two arcades say the lane works; a player whose arcade has not answered
//!   lately has no lane, and the window carries that pair over the room as before;
//! - "d" is data for some players of the room (x), from one player (m), text or bytes (b);
//!   unreliable data waiting to go out is replaced by the newest (positions: only the latest
//!   matters), reliable data keeps its place and order.
//!
//! The receiving arcade hands data only to its own seats in that room that asked for the fast
//! lane, from a player it already knows is there (seen through the room, cert checked).
//!
//! Media ("m", realtime.media) rides the same frames: pieces of encoded pictures and sound that
//! the windows make and play (WebCodecs), opaque here. It goes over the direct link to the
//! player's arcade when there is one; otherwise (two arcades behind NAT with no link between
//! them) through at most MEDIA_HOPS arcades on the room's route, as the original's media went
//! through a node's TURN relay. An arcade passes media on only while its media relay is on
//! (turnsvc.rs: the same switch and the same monthly limit), at most MEDIA_RELAY_BYTES a second
//! for one sending arcade. Media is never replaced by newer pieces (a picture needs every piece
//! since its last key frame), and is dropped instead of queued when the next link is already
//! behind (stale pictures help nobody; the window asks for a key frame and goes on).

use super::*;

/// Most bytes in one message (as the original's fast lane).
pub(super) const FX_MAX: usize = 16 * 1024;
/// Most players one send names.
const FX_TO_MAX: usize = 64;
/// A seat's sends: per second, and the burst.
const FX_RATE: f64 = 240.0;
const FX_BURST: f64 = 480.0;
/// What one arcade passes on for another: frames per second, and the burst.
const FX_RELAY: f64 = 1000.0;
const FX_RELAY_BURST: f64 = 2000.0;
/// Most bytes in one piece of media (a mesh frame is at most 64 KB, header included).
pub(super) const MEDIA_MAX: usize = 60_000;
/// A seat's media: pieces per second and burst, bytes per second and burst.
const MEDIA_RATE: f64 = 600.0;
const MEDIA_BURST: f64 = 1200.0;
const MEDIA_BYTES: f64 = 8_000_000.0;
/// Media for a link with more frames than this waiting is dropped.
const MEDIA_BACKLOG: usize = 600;
/// Arcades media may pass through on its way (two arcades behind NAT, both linked to a third).
const MEDIA_HOPS: u64 = 2;
/// What one arcade passes on of another's media: bytes a second (20 Mbit/s), and the burst.
const MEDIA_RELAY_BYTES: f64 = 2_500_000.0;
/// A lane is up while its arcade was heard within this many seconds.
const LANE_LIFE: u64 = 10;
/// How often a lane is asked about: while down, and while up.
const HI_DOWN: u64 = 2;
const HI_UP: u64 = 4;
/// An fx frame older (or newer) than this is not taken: a fast lane has no use for it.
const FX_SKEW_US: u64 = 10_000_000;

/// When this arcade last queued fast-lane data (ms since the epoch): links send what is
/// queued more often while the lane is busy (crate::meshsvc).
static LAST_FX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The fast lane carried data within the last two seconds.
pub fn busy() -> bool {
    let now = now_us() / 1000;
    now.saturating_sub(LAST_FX.load(std::sync::atomic::Ordering::Relaxed)) < 2000
}

fn note_busy() {
    LAST_FX.store(now_us() / 1000, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Default)]
pub(super) struct Lane {
    /// When this arcade was last heard on the lane (seconds).
    pub heard: u64,
    /// When we last said hi.
    pub asked: u64,
}

fn lane_up(st: &St, origin: &str, now: u64) -> bool {
    st.lanes.get(origin).is_some_and(|l| now.saturating_sub(l.heard) <= LANE_LIFE)
}

/// The link toward arcade `to`: our own link to it, else the way its room traffic comes.
fn hop_to(node: &Node, st: &St, to: &str, room: Option<&str>, not: Option<&str>) -> Option<String> {
    let live: HashSet<String> = link_ids(node).into_iter().collect();
    if live.contains(to) {
        return Some(to.to_string());
    }
    let now = now_s();
    let ok = |hop: &String| live.contains(hop) && Some(hop.as_str()) != not;
    if let Some(r) = room.and_then(|r| st.routes.get(&room_id(r))) {
        if let Some((hop, exp)) = r.get(to) {
            if *exp >= now && ok(hop) {
                return Some(hop.clone());
            }
        }
    }
    st.heard.get(to).filter(|(hop, at)| now.saturating_sub(*at) < ROUTE_LIFE && ok(hop)).map(|(hop, _)| hop.clone())
}

/// Queue an fx frame; unreliable data replaces the same player's unsent unreliable data for the
/// same players (only the newest position matters).
fn fx_queue(st: &mut St, hop: &str, f: Frame) {
    let latest = f.head["k"] == "d" && f.head["rel"] != true;
    let q = st.outq.entry(hop.to_string()).or_default();
    if latest {
        let same = |g: &Frame| {
            g.head["t"] == "fx" && g.head["k"] == "d" && g.head["rel"] != true && g.head["to"] == f.head["to"]
                && g.head["o"] == f.head["o"] && g.head["r"] == f.head["r"] && g.head["m"] == f.head["m"] && g.head["x"] == f.head["x"]
        };
        if let Some(i) = q.iter().position(same) {
            q[i] = f;
            return;
        }
    }
    if q.len() >= OUTQ_MAX {
        q.pop_front();
    }
    q.push_back(f);
}

fn fx_frame(node: &Node, st: &mut St, kind: &str, to: &str, extra: Value, blob: &[u8]) -> Frame {
    let mut h = json!({"t": "fx", "k": kind, "o": my_id(node), "to": to, "q": next_q(st), "hops": 0});
    if let Value::Object(m) = extra {
        for (k, v) in m {
            h[k] = v;
        }
    }
    sign(node, &mut h, blob);
    Frame { head: h, blob: blob.to_vec() }
}

fn say_hi(node: &Node, st: &mut St, to: &str, room: Option<&str>, kind: &str) {
    let Some(hop) = hop_to(node, st, to, room, None) else { return };
    let f = fx_frame(node, st, kind, to, json!({}), b"");
    fx_queue(st, &hop, f);
}

/// The beat (from realtime::run): say hi to the arcades of players in rooms where a seat here
/// wants the fast lane, and ask for a direct link to them.
pub(super) fn beat(node: &Node, st: &mut St) {
    let now = now_s();
    let mut want: BTreeMap<String, String> = BTreeMap::new(); // origin -> a room it is in
    let rooms: HashSet<String> = st.sessions.values().filter(|s| s.fast).map(|s| s.room.clone()).collect();
    for room in &rooms {
        if let Some(by) = st.remote.get(room) {
            for (o, ms) in by {
                if !ms.is_empty() {
                    want.entry(o.clone()).or_insert_with(|| room.clone());
                }
            }
        }
    }
    let links: HashSet<String> = link_ids(node).into_iter().collect();
    for (o, room) in want {
        let up = lane_up(st, &o, now);
        let l = st.lanes.entry(o.clone()).or_default();
        if now.saturating_sub(l.asked) < if up { HI_UP } else { HI_DOWN } {
            continue;
        }
        l.asked = now;
        say_hi(node, st, &o, Some(&room), "hi");
        if !links.contains(&o) {
            crate::udpsvc::want(&o);
        }
    }
    st.lanes.retain(|_, l| now.saturating_sub(l.heard.max(l.asked)) < 600);
    st.fx_seen.retain(|_, at| now.saturating_sub(*at) < 2 * FX_SKEW_US / 1_000_000 + 5);
}

/// realtime.fast: send to players of the seat's room over the fast lane. `to`: the players
/// (ids); each is reached only if this arcade has a lane to the arcade they play through (or
/// they play here).
pub fn send(node: &Node, tok: &str, data: Option<&str>, b64: Option<&str>, to: &[String], reliable: bool) -> Result<Value, String> {
    let (bytes, bin) = match (data, b64) {
        (Some(d), _) => (d.as_bytes().to_vec(), false),
        (None, Some(b)) => (unb64(b)?, true),
        _ => return Err("send data or b64".into()),
    };
    if bytes.len() > FX_MAX {
        return Err(format!("a fast message is at most {FX_MAX} bytes"));
    }
    carry(node, tok, &bytes, bin, to, Some(reliable))
}

/// realtime.media: a piece of the window's encoded pictures or sound for some players of the room,
/// over direct links only.
pub fn media(node: &Node, tok: &str, b64: &str, to: &[String]) -> Result<Value, String> {
    let bytes = unb64(b64)?;
    if bytes.is_empty() || bytes.len() > MEDIA_MAX {
        return Err(format!("a piece of media is 1 to {MEDIA_MAX} bytes"));
    }
    carry(node, tok, &bytes, true, to, None)
}

fn unb64(b: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b).map_err(|_| "b64 is not base64".into())
}

/// Data (reliable: Some) or media (None) from the seat to the players named.
fn carry(node: &Node, tok: &str, bytes: &[u8], bin: bool, to: &[String], reliable: Option<bool>) -> Result<Value, String> {
    if to.is_empty() || to.len() > FX_TO_MAX {
        return Err(format!("to: 1 to {FX_TO_MAX} players"));
    }
    let is_media = reliable.is_none();
    let mut st = rt().st.lock().unwrap();
    let (room, member) = match st.sessions.get(tok) {
        Some(s) if s.fast => (s.room.clone(), s.member.clone()),
        Some(_) => return Err("this seat did not ask for the fast lane".into()),
        None => return Err("join the room first".into()),
    };
    let fits = if is_media {
        allow(&mut st, format!("fm|{tok}"), MEDIA_RATE, MEDIA_BURST) && allow_cost(&mut st, format!("fmb|{tok}"), MEDIA_BYTES, MEDIA_BYTES, bytes.len() as f64)
    } else {
        allow(&mut st, format!("fx|{tok}"), FX_RATE, FX_BURST)
    };
    if !fits {
        return Err("too fast".into());
    }
    let want: HashSet<&str> = to.iter().map(String::as_str).collect();
    let me = my_id(node);
    let now = now_s();
    // Players here: straight to their windows.
    let p = who(node, &me, &member, st.locals.get(&room).and_then(|l| l.members.get(&member)).and_then(|s| s.cred.as_ref()));
    let ev = if is_media { media_event(&room, p, bytes) } else { fast_event(&room, p, bytes, bin) };
    let mut sent = 0;
    for s in st.sessions.values_mut() {
        if s.fast && s.room == room && s.member != member && want.contains(s.member.as_str()) && s.events.len() < 1000 {
            s.events.push_back(ev.clone());
            sent += 1;
        }
    }
    if sent > 0 {
        rt().cv.notify_all();
    }
    // Players elsewhere: one frame per arcade, naming its players.
    let mut by_origin: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(by) = st.remote.get(&room) {
        for (o, ms) in by {
            for m in ms.keys() {
                if want.contains(m.as_str()) {
                    by_origin.entry(o.clone()).or_default().push(m.clone());
                }
            }
        }
    }
    let mut dropped = 0;
    for (o, x) in by_origin {
        if !lane_up(&st, &o, now) {
            continue;
        }
        let Some(hop) = hop_to(node, &st, &o, Some(&room), None) else { continue };
        let f = if is_media {
            // Over the direct link, else through the room's route (on_fx: relaying arcades);
            // never into a link that is already behind.
            if st.outq.get(&hop).is_some_and(|q| q.len() > MEDIA_BACKLOG) {
                dropped += x.len();
                continue;
            }
            fx_frame(node, &mut st, "m", &o, json!({"r": room, "m": member, "x": x}), bytes)
        } else {
            fx_frame(node, &mut st, "d", &o, json!({"r": room, "m": member, "x": x, "rel": reliable == Some(true), "b": bin}), bytes)
        };
        fx_queue(&mut st, &hop, f);
        sent += x.len();
        note_busy();
    }
    Ok(if is_media { json!({"sent": sent, "dropped": dropped}) } else { json!({"sent": sent}) })
}

fn media_event(room: &str, p: Value, bytes: &[u8]) -> Value {
    use base64::Engine as _;
    json!({"type": "mchunk", "room": short_room(room), "member": p, "b64": base64::engine::general_purpose::STANDARD.encode(bytes)})
}

fn fast_event(room: &str, p: Value, bytes: &[u8], bin: bool) -> Value {
    let mut ev = json!({"type": "fast", "room": short_room(room), "member": p});
    if bin {
        use base64::Engine as _;
        ev["b64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
    } else {
        ev["data"] = json!(String::from_utf8_lossy(bytes));
    }
    ev
}

/// realtime.lanes: which players of the seat's room the fast lane reaches now.
pub fn lanes(node: &Node, tok: &str) -> Result<Value, String> {
    let st = rt().st.lock().unwrap();
    let Some(s) = st.sessions.get(tok) else { return Err("join the room first".into()) };
    let (room, member) = (s.room.clone(), s.member.clone());
    let now = now_s();
    let mut up: BTreeSet<String> = st.sessions.values().filter(|o| o.fast && o.room == room && o.member != member).map(|o| o.member.clone()).collect();
    if let Some(by) = st.remote.get(&room) {
        for (o, ms) in by {
            if lane_up(&st, o, now) {
                up.extend(ms.keys().cloned());
            }
        }
    }
    let direct: BTreeSet<String> = link_ids(node).into_iter().collect();
    let routes: serde_json::Map<String, Value> = st
        .remote
        .get(&room)
        .map(|by| {
            by.iter()
                .filter(|(o, _)| lane_up(&st, o, now))
                .flat_map(|(o, ms)| {
                    let how = if direct.contains(o) { "direct" } else { "relay" };
                    ms.keys().map(move |m| (m.clone(), json!(how)))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"up": up, "routes": routes}))
}

/// An fx frame from the link to arcade `from`.
pub(super) fn on_fx(node: &Node, st: &mut St, from: &str, h: &Value, blob: &[u8]) {
    let hops = h["hops"].as_u64().unwrap_or(99);
    let (Some(to), Some(origin), Some(kind)) = (h["to"].as_str(), h["o"].as_str(), h["k"].as_str()) else { return };
    if hops >= MAX_HOPS || blob.len() > if kind == "m" { MEDIA_MAX } else { FX_MAX } || !["hi", "ho", "d", "m"].contains(&kind) {
        return;
    }
    let me = my_id(node);
    if origin == me {
        return;
    }
    if to != me {
        // For another arcade: pass it on, toward it, never back.
        if hops + 1 >= MAX_HOPS || !allow(st, format!("fxr|{origin}"), FX_RELAY, FX_RELAY_BURST) {
            return;
        }
        let Some(hop) = hop_to(node, st, to, h["r"].as_str(), Some(from)) else { return };
        // Media: a few arcades on its way at most, within this arcade's relay budget, and never
        // into a link that is already behind.
        if kind == "m"
            && (hops >= MEDIA_HOPS
                || st.outq.get(&hop).is_some_and(|q| q.len() > MEDIA_BACKLOG)
                || !allow_cost(st, format!("fmr|{origin}"), MEDIA_RELAY_BYTES, MEDIA_RELAY_BYTES, blob.len() as f64)
                || !crate::turnsvc::charge(blob.len()))
        {
            return;
        }
        let mut c = h.clone();
        c["hops"] = json!(hops + 1);
        fx_queue(st, &hop, Frame { head: c, blob: blob.to_vec() });
        note_busy();
        return;
    }
    // Each frame is taken once (frames can come out of order: two routes, or newer data put in
    // the place of older), and only while fresh.
    let Some(q) = h["q"].as_u64() else { return };
    let key = format!("{origin}:{q}");
    if q.abs_diff(now_us()) > FX_SKEW_US || st.fx_seen.contains_key(&key) || !verify(h, blob) {
        return;
    }
    let now = now_s();
    st.fx_seen.insert(key, now);
    st.lanes.entry(origin.to_string()).or_default().heard = now;
    match kind {
        "hi" => {
            let f = fx_frame(node, st, "ho", origin, json!({}), b"");
            fx_queue(st, from, f);
        }
        "ho" => {}
        _ => {
            let (Some(room), Some(member)) = (h["r"].as_str(), h["m"].as_str()) else { return };
            let Some(x) = h["x"].as_array() else { return };
            let x: HashSet<&str> = x.iter().filter_map(|v| v.as_str()).collect();
            let Some(known) = st.remote.get(room).and_then(|r| r.get(origin)).and_then(|ms| ms.get(member)).cloned() else { return };
            if !vet(node, st, origin, member, known.cred.as_ref()) {
                return;
            }
            let p = who(node, origin, member, known.cred.as_ref());
            let ev = if kind == "m" { media_event(room, p, blob) } else { fast_event(room, p, blob, h["b"] == true) };
            let mut told = false;
            for s in st.sessions.values_mut() {
                if s.fast && s.room == room && x.contains(s.member.as_str()) && s.events.len() < 1000 {
                    s.events.push_back(ev.clone());
                    told = true;
                }
            }
            if told {
                rt().cv.notify_all();
            }
        }
    }
}

/// A seat that asked for the fast lane (realtime.join with fast).
pub fn want_fast(tok: &str) {
    if let Some(s) = rt().st.lock().unwrap().sessions.get_mut(tok) {
        s.fast = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(k: &str, m: &str, rel: bool, n: u8) -> Frame {
        Frame { head: json!({"t": "fx", "k": k, "o": "a", "to": "b", "r": "g/r", "m": m, "x": ["p"], "rel": rel}), blob: vec![n] }
    }

    #[test]
    fn only_the_newest_unreliable_message_waits() {
        let mut st = St::default();
        fx_queue(&mut st, "hop", frame("d", "m1", false, 1));
        fx_queue(&mut st, "hop", frame("d", "m1", true, 2));
        fx_queue(&mut st, "hop", frame("d", "m1", false, 3));
        fx_queue(&mut st, "hop", frame("d", "m2", false, 4));
        fx_queue(&mut st, "hop", frame("d", "m1", true, 5));
        fx_queue(&mut st, "hop", frame("d", "m1", false, 6));
        let got: Vec<u8> = st.outq["hop"].iter().map(|f| f.blob[0]).collect();
        // m1's unreliable 1 became 3 then 6, in its first place; reliable ones keep theirs.
        assert_eq!(got, vec![6, 2, 4, 5]);
    }

    #[test]
    fn a_hi_is_never_replaced() {
        let mut st = St::default();
        fx_queue(&mut st, "hop", frame("hi", "", false, 1));
        fx_queue(&mut st, "hop", frame("hi", "", false, 2));
        assert_eq!(st.outq["hop"].len(), 2);
    }
}

#[cfg(test)]
mod conn_tests {
    use super::super::*;

    fn st_with(peer: &str, conns: &[u64]) -> St {
        let mut st = St::default();
        st.conns.insert(peer.into(), conns.to_vec());
        st.outq.entry(peer.into()).or_default().push_back(Frame::new(json!({"t": "pub"})));
        st.routes.entry("r".into()).or_default().insert("o".into(), (peer.into(), u64::MAX));
        st
    }

    #[test]
    fn room_frames_go_on_the_link_the_peer_talks_on() {
        let mut st = st_with("p", &[1, 2]);
        assert_eq!(carrier(&st, "p"), Some(1), "none heard yet: the oldest");
        st.via_conn.insert("p".into(), 2);
        assert_eq!(carrier(&st, "p"), Some(2), "its room frames came on 2");
        st.via_conn.insert("p".into(), 9);
        assert_eq!(carrier(&st, "p"), Some(1), "a link that is gone carries nothing");
    }

    #[test]
    fn a_second_link_closing_keeps_the_routes() {
        let mut st = st_with("p", &[1, 2]);
        st.via_conn.insert("p".into(), 1);
        drop_conn(&mut st, "p", 2);
        assert_eq!(st.routes["r"].len(), 1, "a borrowing connection closing leaves the room's route");
        assert_eq!(st.outq["p"].len(), 1);
        drop_conn(&mut st, "p", 1);
        assert!(st.routes["r"].is_empty() && !st.outq.contains_key("p"), "the last link closing takes them");
    }
}
