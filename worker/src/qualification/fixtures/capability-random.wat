(component
  (import "wasi:random/random@0.2.0" (instance $random
    EXTRA
    (export "get-random-bytes" (func (param "len" u64) (result (list u8))))
    (export "get-random-u64" (func (result u64)))))
  (core module $memory
    (memory (export "memory") 4 4)
    (global $next (mut i32) (i32.const 4096))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $pointer i32)
      (local.set $pointer (global.get $next))
      (global.set $next (i32.add (global.get $next) (local.get 3)))
      (local.get $pointer)))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias core export $memory "realloc" (core func $realloc))
  (alias export $random "get-random-bytes" (func $bytes))
  (alias export $random "get-random-u64" (func $u64))
  (core func $bytes (canon lower (func $bytes) (memory $memory) (realloc $realloc)))
  (core func $u64 (canon lower (func $u64)))
  (core module $guest
    (import "host" "memory" (memory 4 4))
    (import "host" "bytes" (func $bytes (param i64 i32)))
    (import "host" "u64" (func $u64 (result i64)))
    (func (export "u64") (result i64) (call $u64))
    ;; Returns the length of the list the host lowered at the return area.
    (func (export "bytes") (param $len i64) (result i32)
      (call $bytes (local.get $len) (i32.const 0))
      (i32.load offset=4 (i32.const 0)))
    ;; 1 when two draws of `len` bytes differ somewhere, else 0.
    (func (export "differs") (param $len i64) (result i32)
      (local $a i32) (local $b i32) (local $i i32) (local $n i32)
      (call $bytes (local.get $len) (i32.const 0))
      (call $bytes (local.get $len) (i32.const 8))
      (local.set $a (i32.load (i32.const 0)))
      (local.set $b (i32.load (i32.const 8)))
      (local.set $n (i32.load offset=4 (i32.const 0)))
      (block $done
        (loop $next
          (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
          (if (i32.ne (i32.load8_u (i32.add (local.get $a) (local.get $i)))
                      (i32.load8_u (i32.add (local.get $b) (local.get $i))))
            (then (return (i32.const 1))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next)))
      (i32.const 0))
    ;; 1 when the bytes of one draw are not all equal, else 0.
    (func (export "varied") (param $len i64) (result i32)
      (local $a i32) (local $i i32) (local $n i32)
      (call $bytes (local.get $len) (i32.const 0))
      (local.set $a (i32.load (i32.const 0)))
      (local.set $n (i32.load offset=4 (i32.const 0)))
      (block $done
        (loop $next
          (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
          (if (i32.ne (i32.load8_u (i32.add (local.get $a) (local.get $i)))
                      (i32.load8_u (local.get $a)))
            (then (return (i32.const 1))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next)))
      (i32.const 0)))
  (core instance $host
    (export "memory" (memory $memory)) (export "bytes" (func $bytes)) (export "u64" (func $u64)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func (export "u64") (result u64) (canon lift (core func $guest "u64")))
  (func (export "bytes") (param "len" u64) (result u32) (canon lift (core func $guest "bytes")))
  (func (export "differs") (param "len" u64) (result u32) (canon lift (core func $guest "differs")))
  (func (export "varied") (param "len" u64) (result u32) (canon lift (core func $guest "varied"))))
