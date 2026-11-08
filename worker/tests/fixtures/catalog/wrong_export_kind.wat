;; Handler name exported as a type, not an instance: missing_handler_export.
(component
  (type $host (instance))
  (core module $m
    (memory (export "memory") 18)
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $ptr i32)
      (if (i32.gt_u (local.get 3) (i32.sub (i32.const 1179648) (global.get $heap)))
        (then unreachable))
      (local.set $ptr (global.get $heap))
      (global.set $heap (i32.add (local.get $ptr) (local.get 3)))
      (local.get $ptr))
    ;; result<list<u8>, handler-error>: tag at 16, list ptr at 20, list len at 24.
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.store (i32.const 20) (local.get $ptr))
      (i32.store (i32.const 24) (local.get $len))
      (i32.const 16)))
  (core instance $i (instantiate $m))
  (type $handler-error (variant (case "invalid-input" string) (case "failed" string)))
  (type $handle-type (func (param "params" (list u8)) (result (result (list u8) (error $handler-error)))))
  (func $handle (type $handle-type)
    (canon lift (core func $i "handle") (memory $i "memory") (realloc (func $i "realloc"))))
  (instance $handler
    (export "handler-error" (type $handler-error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (type $handler-error))
)
