(component
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  ;; A second export that would hand the guest memory to its embedder: it is refused, so the
  ;; only way out of the guest is the handler.
  (core module $memory (memory (export "memory") 64))
  (core module $guest
    (memory (export "memory") 64)
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      i32.const 4 local.get $ptr i32.store
      i32.const 8 local.get $len i32.store
      i32.const 0)
  )
  (core instance $guest (instantiate $guest))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
  (export "memory-module" (core module $memory))
)
