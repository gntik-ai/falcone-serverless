(component
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
  (func (export "bytes-len") (param "len" u64) (result u32) (canon lift (core func $guest "bytes-len"))))
