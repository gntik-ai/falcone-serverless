use falcone_serverless_worker::host::{self, Bindings, Context, Host, HostError, Operation, Reply};
use std::sync::{Arc, Mutex};

/// Records the invocation's log lines; WASI reaches no other platform binding.
#[derive(Default)]
pub struct Captured(Mutex<Vec<String>>);
impl Bindings for Captured {
    fn call(&self, _: &Context, operation: Operation) -> Result<Reply, HostError> {
        match operation {
            Operation::Log { message, fields, .. } => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name, "stream");
                assert_eq!(fields[0].value, "stdout");
                self.0.lock().unwrap().push(message);
                Ok(Reply::Empty)
            }
            _ => panic!("WASI must not call other platform bindings"),
        }
    }
}
fn host() -> Host {
    Host::new(Context::default(), Arc::new(Captured::default()))
}

pub fn csprng_is_linked_narrowly_and_bounded() {
    let source = r#"(component
      (import "wasi:random/random@0.2.0" (instance $random
        (export "get-random-u64" (func (result u64)))
        (export "get-random-bytes" (func (param "len" u64) (result (list u8))))))
      (core module $mem
        (memory (export "memory") 2)
        (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024))
      (core instance $mem (instantiate $mem))
      (alias export $random "get-random-u64" (func $draw))
      (alias export $random "get-random-bytes" (func $bytes))
      (core func $draw (canon lower (func $draw)))
      (core func $bytes (canon lower (func $bytes) (memory $mem "memory") (realloc (func $mem "realloc"))))
      (core module $guest
        (import "host" "draw" (func $draw (result i64)))
        (import "host" "bytes" (func $bytes (param i64 i32)))
        (import "host" "memory" (memory 2))
        (func (export "draw") (result i64) call $draw)
        (func (export "bytes-len") (param i64) (result i32)
          local.get 0 i32.const 0 call $bytes i32.const 4 i32.load))
      (core instance $guest (instantiate $guest (with "host" (instance
        (export "draw" (func $draw)) (export "bytes" (func $bytes))
        (export "memory" (memory $mem "memory"))))))
      (func (export "draw") (result u64) (canon lift (core func $guest "draw")))
      (func (export "bytes-len") (param "len" u64) (result u32) (canon lift (core func $guest "bytes-len"))))"#;
    let engine = wasmtime::Engine::default();
    let (mut store, instance) = host::instantiate(&engine, source, host()).unwrap();
    let draw = instance
        .get_typed_func::<(), (u64,)>(&mut store, "draw")
        .unwrap();
    let a = draw.call(&mut store, ()).unwrap().0;
    let b = draw.call(&mut store, ()).unwrap().0;
    assert_ne!(a, b);
    let bytes = instance
        .get_typed_func::<(u64,), (u32,)>(&mut store, "bytes-len")
        .unwrap();
    assert_eq!(bytes.call(&mut store, (65536,)).unwrap().0, 65536);
    assert!(bytes.call(&mut store, (65537,)).is_err());
    let unlinked = source.replace("get-random-u64", "unlinked");
    assert!(host::instantiate(&engine, unlinked, host()).is_err());
}

pub fn stdout_is_bounded_and_invocation_local() {
    let source = include_str!("../fixtures/go/wasi-stdout.wat");
    let invoke = |length, blocking, barrier: Option<&std::sync::Barrier>| {
        let engine = wasmtime::Engine::default();
        let captured = Arc::new(Captured::default());
        let host = Host::new(Context::default(), captured.clone());
        let (mut store, instance) = host::instantiate(&engine, source, host).unwrap();
        let write = instance
            .get_typed_func::<(u32, u32), (u64,)>(&mut store, "write")
            .unwrap();
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let remaining = write.call(&mut store, (length, blocking)).unwrap().0;
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        // The fixture writes zero bytes, so each captured byte is one NUL character.
        let lines = captured.0.lock().unwrap().clone();
        assert!(lines.iter().all(|line| line.bytes().all(|byte| byte == 0)));
        let logged: usize = lines.iter().map(String::len).sum();
        (remaining, store.data().stdout_summary(), logged)
    };
    // Captured in the invocation's log record, up to the bound, then truncated.
    assert_eq!(invoke(4, 0, None), (65532, (4, false), 4));
    assert_eq!(invoke(65536, 0, None), (0, (65536, false), 65536));
    assert_eq!(invoke(65537, 1, None), (0, (65536, true), 65536));
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| invoke(123, 0, Some(&barrier)));
        let b = scope.spawn(|| invoke(456, 1, Some(&barrier)));
        assert_eq!(a.join().unwrap(), (65413, (123, false), 123));
        assert_eq!(b.join().unwrap(), (65080, (456, false), 456));
    });
}

pub fn one_memory_and_only_declared_functions_are_linked() {
    let engine = wasmtime::Engine::default();
    let two_memories = r#"(component
      (core module $mem (memory 1))
      (core instance (instantiate $mem))
      (core instance (instantiate $mem)))"#;
    assert!(host::instantiate(&engine, two_memories, host()).is_err());
    let unlinked = include_str!("../fixtures/go/wasi-stdout.wat").replace(
        "[method]output-stream.write",
        "[method]output-stream.unlinked",
    );
    assert!(host::instantiate(&engine, unlinked, host()).is_err());
}
