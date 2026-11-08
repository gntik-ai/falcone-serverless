;; Import-free guest whose handle returns its params unchanged.
(component
  (core module $m
    (memory (export "memory") 2)
    (global $heap (mut i32) (i32.const 1024))
    ;; Bump allocator: the host copies params in, and the echo returns them in place.
    (func (export "cabi_realloc") (param $old i32) (param $old_size i32) (param $align i32) (param $size i32) (result i32)
      (local $ptr i32)
      (local.set $ptr
        (i32.and
          (i32.add (global.get $heap) (i32.sub (local.get $align) (i32.const 1)))
          (i32.sub (i32.const 0) (local.get $align))))
      (global.set $heap (i32.add (local.get $ptr) (local.get $size)))
      (local.get $ptr))
    ;; result<list<u8>, handler-error> in memory: tag, then pointer and length.
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (local.get $ptr))
      (i32.store (i32.const 8) (local.get $len))
      (i32.const 0)))
  (core instance $i (instantiate $m))
  (type $error-def (variant (case "invalid-input" string) (case "failed" string)))
  (instance $types (export "handler-error" (type $error-def)))
  (alias export $types "handler-error" (type $error))
  (type $outcome (result (list u8) (error $error)))
  (func $handle (param "params" (list u8)) (result $outcome)
    (canon lift (core func $i "handle") (memory $i "memory") (realloc (func $i "cabi_realloc"))))
  (instance $handler (export "handler-error" (type $error)) (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))
