(component
  (core module $guest
    (memory (export "memory") 64 64)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (i32.store (i32.const 32) (i32.const 0))
      (i32.store (i32.const 36) (local.get $ptr))
      (i32.store (i32.const 40) (local.get $len))
      i32.const 32))
  (core instance $guest (instantiate $guest))
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (func $handle (param "params" $bytes) (result $result)
    (canon lift (core func $guest "handle") (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))
