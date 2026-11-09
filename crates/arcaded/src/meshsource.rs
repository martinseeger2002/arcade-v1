//! A V1.0 arcade on the mesh as a block source for sync (mesh-first bootstrap).

use arcade_chain::p2p::{parse_headers, HeaderEntry};
use arcade_chain::Hash256;
use arcade_chaindb::sync::BlockSource;
use arcade_mesh::bk;
use arcade_mesh::link::{Frame, Link};
use serde_json::json;
use std::time::Duration;

pub struct MeshSource {
    link: Link,
    addr: String,
    net: &'static str,
    format: arcade_chain::Format,
    next_id: u64,
}

impl MeshSource {
    pub fn dial(node: &crate::node::Node, addr: &str) -> Result<MeshSource, String> {
        Self::dial_chain(node, addr, node.params)
    }

    /// A V1.0 arcade's blocks of any chain.
    pub fn dial_chain(node: &crate::node::Node, addr: &str, p: &'static arcade_chain::params::ChainParams) -> Result<MeshSource, String> {
        let link = Link::dial_asking(addr, crate::meshsvc::NET, &node.identity, node.mesh_port, Duration::from_secs(20))?;
        Ok(MeshSource { link, addr: addr.into(), net: p.net, format: p.format, next_id: 1 })
    }

    fn ask(&mut self, mut head: serde_json::Value) -> Result<Vec<u8>, String> {
        let id = self.next_id;
        self.next_id += 1;
        head["id"] = json!(id);
        self.link.send(&Frame::new(head)).map_err(|e| e.to_string())?;
        bk::read_answer(&mut self.link, id)
    }
}

impl BlockSource for MeshSource {
    fn get_headers(&mut self, locator: &[Hash256]) -> Result<Vec<HeaderEntry>, String> {
        let loc: Vec<String> = locator.iter().map(|h| h.to_hex()).collect();
        let data = self.ask(json!({"t": "bk", "op": "hq", "net": self.net, "loc": loc}))?;
        parse_headers(&data, &self.format).map_err(|e| e.to_string())
    }

    fn get_blocks(&mut self, hashes: &[Hash256]) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(bk::MAX_BLOCKS) {
            let h: Vec<String> = chunk.iter().map(|x| x.to_hex()).collect();
            let data = self.ask(json!({"t": "bk", "op": "bq", "net": self.net, "h": h}))?;
            out.extend(bk::split_blocks(&data)?);
        }
        Ok(out)
    }

    fn describe(&self) -> String {
        format!("mesh {}", self.addr)
    }
}
