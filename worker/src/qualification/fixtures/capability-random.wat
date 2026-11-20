(component
  (import "wasi:random/random@0.2.0" (instance $random
    (type $bytes (list u8))
    (export "get-random-u64" (func (result u64)))
    (export "get-random-bytes" (func (param "len" u64) (result $bytes)))))
  (alias export $random "get-random-u64" (func $u64))
  (alias export $random "get-random-bytes" (func $bytes))
  (core module $memory
    (memory (export "memory") 2)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 16))
  (core instance $memory (instantiate $memory))
  (core func $u64 (canon lower (func $u64)))
  (core func $bytes (canon lower (func $bytes) (memory $memory "memory") (realloc (func $memory "realloc"))))
  (core module $guest
    (import "host" "u64" (func $u64 (result i64)))
    (import "host" "bytes" (func $bytes (param i64 i32)))
    (import "host" "memory" (memory 2))
    (func (export "different") (result i32) call $u64 call $u64 i64.ne)
    (func (export "bytes") (param i64) (result i32)
      local.get 0 i32.const 0 call $bytes i32.const 4 i32.load))
  (core instance $guest (instantiate $guest (with "host" (instance
    (export "u64" (func $u64)) (export "bytes" (func $bytes))
    (export "memory" (memory $memory "memory"))))))
  (func (export "different") (result bool) (canon lift (core func $guest "different")))
  (func (export "bytes") (param "len" u64) (result u32) (canon lift (core func $guest "bytes"))))
