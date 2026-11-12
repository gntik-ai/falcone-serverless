(component
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (memory (export "memory") 64)
    (global $heap (mut i32) (i32.const 2048))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    ;; A fixed legacy handler result, whatever the params are.
    (data (i32.const 1024) "{\"statusCode\":200,\"body\":{\"ok\":true}}")
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      i32.const 4 i32.const 1024 i32.store
      i32.const 8 i32.const 37 i32.store
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
