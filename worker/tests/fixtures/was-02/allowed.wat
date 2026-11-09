(component
  (type $host (instance))
  (import "falcone:host/storage@0.2.0" (instance (type $host)))
  (import "falcone:host/http@0.2.0" (instance (type $host)))
  (import "falcone:host/events@0.2.0" (instance (type $host)))
  (import "falcone:host/workflows@0.2.0" (instance (type $host)))
  (import "falcone:host/logging@0.2.0" (instance (type $host)))
  (import "falcone:host/cancellation@0.2.0" (instance (type $host)))
  (import "falcone:host/progress@0.2.0" (instance (type $host)))
  (import "falcone:host/invocation-context@0.2.0" (instance (type $host)))
  (core module (func (export "answer") (result i32) i32.const 42))
 (component $handler
  ;; IMPORTS
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    ;; CORE_IMPORTS
    (memory (export "memory") 64)
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      ;; BODY
      i32.const 4 local.get $ptr i32.store
      i32.const 8 local.get $len i32.store
      i32.const 0)
  )
  (core instance $guest (instantiate $guest ;; CORE_ARGS
  ))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
) (instance $handler (instantiate $handler)) (alias export $handler "falcone:host/handler@0.2.0" (instance $export)) (export "falcone:host/handler@0.2.0" (instance $export)))
