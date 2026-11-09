;; One large function keeps compiler address-space pressure distinct from input size.
;; Expand OPERATIONS to 131072 multiply/add pairs; never execute this function.
(component (core module
  (func (param i32) (result i32)
    local.get 0
    ;; OPERATIONS
  )
) (component $handler
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
