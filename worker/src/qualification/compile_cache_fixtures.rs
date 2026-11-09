//! Fixed compilation-only qualification workloads; never instantiate them.
use crate::artifact::supervisor::CompileLimits;
use std::time::{Duration, Instant};

// Keep binaries self-contained in the image build's source-only copy set.
pub const NATIVE: &[u8] = b"\x7fELF\x02\x01\x01\0";
pub const INVALID: &[u8] = b"\0asm\x0d\0\x01\0\xff";

pub fn limits() -> CompileLimits {
    CompileLimits {
        input_bytes: 64 * 1024 * 1024,
        cpu_seconds: 120,
        memory_bytes: 4 * 1024 * 1024 * 1024,
        wall_clock_deadline: Instant::now() + Duration::from_secs(300),
    }
}

pub fn small() -> Vec<u8> {
    include_bytes!("execute_fixtures/echo.wasm").to_vec()
}

pub fn cpu() -> Vec<u8> {
    component(64, 8192)
}

pub fn memory() -> Vec<u8> {
    component(1, 131072)
}

// Encode only the fixed fixture grammar, avoiding a production parser dependency
// and megabytes of repeated checked-in bytes. Tests compare these bytes to the
// independently parsed WAT fixtures. This is never an artifact intake encoder.
fn leb(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 127) as u8;
        value >>= 7;
        out.push(byte | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}

fn section(id: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(id);
    leb(payload.len(), out);
    out.extend_from_slice(payload);
}

fn component(functions: usize, operations: usize) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    section(1, &[1, 0x60, 1, 0x7f, 1, 0x7f], &mut module);
    let mut declarations = Vec::new();
    leb(functions, &mut declarations);
    declarations.resize(declarations.len() + functions, 0);
    section(3, &declarations, &mut module);
    let mut body = vec![0, 0x20, 0]; // No locals; local.get 0.
    for _ in 0..operations {
        body.extend_from_slice(&[0x41, 3, 0x6c, 0x41, 1, 0x6a]);
    }
    body.push(0x0b);
    let mut code = Vec::new();
    leb(functions, &mut code);
    for _ in 0..functions {
        leb(body.len(), &mut code);
        code.extend_from_slice(&body);
    }
    section(10, &code, &mut module);
    let mut component = small();
    section(1, &module, &mut component);
    component
}
