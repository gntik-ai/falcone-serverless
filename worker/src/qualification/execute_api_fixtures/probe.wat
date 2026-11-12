(component
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (memory (export "memory") 16)
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    ;; The handler result's body is the hex of the first 0x30000 bytes of this guest's memory,
    ;; read after the params were copied in: whatever the guest holds, the caller can scan.
    (data (i32.const 0x40000) "{\"statusCode\":200,\"body\":\"")
    (func $hex (param $n i32) (result i32)
      local.get $n
      i32.const 48 i32.const 87 local.get $n i32.const 10 i32.lt_u select
      i32.add)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $i i32) (local $o i32) (local $b i32)
      i32.const 0x40000 i32.const 26 i32.add local.set $o
      (loop $next
        local.get $i i32.load8_u local.set $b
        local.get $o local.get $b i32.const 4 i32.shr_u call $hex i32.store8
        local.get $o i32.const 1 i32.add local.get $b i32.const 15 i32.and call $hex i32.store8
        local.get $o i32.const 2 i32.add local.set $o
        local.get $i i32.const 1 i32.add local.tee $i
        i32.const 0x30000 i32.lt_u br_if $next)
      local.get $o i32.const 34 i32.store8
      local.get $o i32.const 1 i32.add i32.const 125 i32.store8
      i32.const 4 i32.const 0x40000 i32.store
      i32.const 8 local.get $o i32.const 2 i32.add i32.const 0x40000 i32.sub i32.store
      i32.const 0)
  )
  (core instance $guest (instantiate $guest))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
)
