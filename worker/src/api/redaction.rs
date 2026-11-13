//! Byte-alphabet Aho-Corasick with union-of-matches redaction. The fixed alphabet
//! makes construction and scanning linear, including overlaps, without a crate.
use super::ExecuteRequest;
use serde_json::Value;
use std::collections::VecDeque;

const MARKER: &str = "<redacted>";
const MAX_SECRET_BYTES: usize = 262_144;
const MAX_NODES: usize = 16_384;
const WORK: usize = 32 * 1024 * 1024;
struct Node {
    next: [u32; 256],
    fail: usize,
    depth: usize,
    matched: usize,
}
impl Node {
    fn new(depth: usize) -> Self {
        Self {
            next: [0; 256],
            fail: 0,
            depth,
            matched: 0,
        }
    }
}
struct Matcher {
    nodes: Vec<Node>,
    work: usize,
}
impl Matcher {
    fn charge(&mut self, n: usize) -> Result<(), ()> {
        self.work = self.work.checked_sub(n).ok_or(())?;
        Ok(())
    }
    fn add(&mut self, secret: &str) -> Result<(), ()> {
        self.charge(secret.len())?;
        if secret.is_empty() {
            return Ok(());
        }
        let mut at = 0;
        for byte in secret.bytes() {
            let edge = self.nodes[at].next[byte as usize];
            at = if edge != 0 {
                edge as usize
            } else {
                if self.nodes.len() == MAX_NODES {
                    return Err(());
                }
                let next = self.nodes.len();
                self.nodes.push(Node::new(self.nodes[at].depth + 1));
                self.nodes[at].next[byte as usize] = next as u32;
                next
            };
        }
        self.nodes[at].matched = secret.len();
        Ok(())
    }
    fn collect(&mut self, value: &Value) -> Result<(), ()> {
        self.charge(1)?;
        match value {
            Value::String(s) => {
                self.add(s)?;
                let encoded = serde_json::to_string(s).map_err(|_| ())?;
                self.add(&encoded[1..encoded.len() - 1])
            }
            Value::Array(values) => {
                for value in values {
                    self.collect(value)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                for value in values.values() {
                    self.collect(value)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn build(&mut self) -> Result<(), ()> {
        let mut queue: VecDeque<_> = self.nodes[0]
            .next
            .iter()
            .copied()
            .filter(|n| *n != 0)
            .map(|n| n as usize)
            .collect();
        while let Some(at) = queue.pop_front() {
            self.charge(256)?;
            let fail = self.nodes[at].fail;
            self.nodes[at].matched = self.nodes[at].matched.max(self.nodes[fail].matched);
            for byte in 0..256 {
                let next = self.nodes[at].next[byte] as usize;
                let fallback = self.nodes[fail].next[byte];
                if next == 0 {
                    self.nodes[at].next[byte] = fallback;
                } else {
                    self.nodes[next].fail = fallback as usize;
                    queue.push_back(next);
                }
            }
        }
        Ok(())
    }
    fn line(&mut self, source: &str) -> Result<String, ()> {
        // Never scan the unbounded suffix. A truncated secret prefix at the end
        // is covered too, so truncation cannot expose part of a credential.
        let end = source
            .char_indices()
            .nth(8192)
            .map(|(at, _)| at)
            .unwrap_or(source.len());
        let line = &source[..end];
        self.charge(line.len().saturating_mul(2))?;
        let mut cover = vec![0i32; line.len() + 1];
        let mut state = 0;
        for (at, byte) in line.bytes().enumerate() {
            state = self.nodes[state].next[byte as usize] as usize;
            let len = self.nodes[state].matched;
            if len > 0 {
                cover[at + 1 - len] += 1;
                cover[at + 1] -= 1;
            }
        }
        // The bounded structured-log collector can already have truncated this
        // line. Cover a final prefix even when no suffix is available here.
        if self.nodes[state].depth > 0 {
            cover[end - self.nodes[state].depth] += 1;
            cover[end] -= 1;
        }
        let mut output = String::new();
        let mut active = 0;
        let mut hidden = false;
        for (at, ch) in line.char_indices() {
            // Match boundaries are UTF-8 boundaries; accumulate all byte deltas
            // since the preceding character, including continuation bytes.
            active += cover[at];
            if active > 0 {
                if !hidden {
                    output.push_str(MARKER);
                }
                hidden = true;
            } else {
                output.push(ch);
                hidden = false;
            }
            for delta in &cover[at + 1..at + ch.len_utf8()] {
                active += delta;
            }
        }
        Ok(output.chars().take(8192).collect())
    }
}
impl ExecuteRequest {
    /// Fixed 32 Mi work budget, 256 KiB serialized secret set, 16,384 states.
    /// Exceeding any bound drops *all* lines, including previously redacted ones.
    pub fn redact_logs(&self, lines: &[String]) -> Vec<String> {
        let redact = || -> Result<Vec<String>, ()> {
            let bytes = serde_json::to_vec(&self.credential).map_err(|_| ())?;
            if bytes.len() > MAX_SECRET_BYTES {
                return Err(());
            }
            let secrets: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
            let mut matcher = Matcher {
                nodes: vec![Node::new(0)],
                work: WORK,
            };
            matcher.collect(&secrets)?;
            matcher.build()?;
            lines
                .iter()
                .take(1000)
                .map(|line| matcher.line(line))
                .collect()
        };
        redact().unwrap_or_else(|()| vec![MARKER.into()])
    }
}
