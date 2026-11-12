use falcone_serverless_worker::{config, server};
use serde_json::Value;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub fn request() -> Value {
    serde_json::from_str(include_str!(
        "../fixtures/contract/request/valid/wasm-no-base.json"
    ))
    .unwrap()
}

pub struct Service {
    pub inner: Arc<server::Service>,
    pub root: PathBuf,
    log: Mutex<Vec<u8>>,
}
impl Service {
    pub fn start(concurrency: u32) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("execute-api-{}-{id}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let config = config::load_from_str(&format!("pool_id = 'test'\nfalcone_api_endpoint = 'https://invalid.test'\nfalcone_dispatcher_endpoint = 'https://invalid.test'\nconcurrency = {concurrency}\ncache_path = '{}'\ncancel_reserve_ms = 1000\n", root.display())).unwrap();
        Self {
            inner: server::Service::new(config).unwrap(),
            root,
            log: Mutex::new(Vec::new()),
        }
    }
    pub fn post(&self, path: &str, input: &Value) -> (u16, Value) {
        let mut io = Wire::new(path, input);
        let connection = if matches!(path, "/v1/cancel" | "/v1/update") {
            self.inner.control_connection()
        } else {
            self.inner.connection()
        };
        let Some(connection) = connection else {
            return (503, serde_json::json!({"code":"busy"}));
        };
        connection.respond(&mut io, &mut *self.log.lock().unwrap());
        io.response()
    }
    pub fn logs(&self) -> String {
        String::from_utf8(self.log.lock().unwrap().clone()).unwrap()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
pub struct Wire {
    pub input: Cursor<Vec<u8>>,
    pub output: Vec<u8>,
}
impl Wire {
    pub fn new(path: &str, input: &Value) -> Self {
        let body = serde_json::to_vec(input).unwrap();
        let mut input = format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        input.extend(body);
        Self {
            input: Cursor::new(input),
            output: Vec::new(),
        }
    }
    pub fn response(&self) -> (u16, Value) {
        let text = std::str::from_utf8(&self.output).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        (
            head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            serde_json::from_str(body).unwrap(),
        )
    }
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
