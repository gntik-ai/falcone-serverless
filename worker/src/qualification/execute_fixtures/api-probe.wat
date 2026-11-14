(component
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (memory (export "memory") 64)
    ;; Two answers, and the needle: a string of the credential, placed at 3 MiB.
    (data (i32.const 2097152) "{\"statusCode\":200,\"body\":\"absent\"}")
    (data (i32.const 2097216) "{\"statusCode\":200,\"body\":\"present\"}")
    (data (i32.const 3145728) "issuer-canary-local")
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    ;; Whether the needle occurs at $at.
    (func $matches (param $at i32) (result i32)
      (local $k i32)
      (loop $next
        (if (i32.ne (i32.load8_u (i32.add (local.get $at) (local.get $k)))
                    (i32.load8_u (i32.add (i32.const 3145728) (local.get $k))))
          (then (return (i32.const 0))))
        (local.set $k (i32.add (local.get $k) (i32.const 1)))
        (br_if $next (i32.lt_u (local.get $k) (i32.const 19))))
      (i32.const 1))
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $i i32) (local $found i32)
      ;; Everything below the needle, then everything above it.
      (local.set $i (i32.const 0))
      (loop $below
        (if (call $matches (local.get $i)) (then (local.set $found (i32.const 1))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $below (i32.lt_u (local.get $i) (i32.const 3145700))))
      (local.set $i (i32.const 3145760))
      (loop $above
        (if (call $matches (local.get $i)) (then (local.set $found (i32.const 1))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $above (i32.lt_u (local.get $i) (i32.const 4194280))))
      (i32.store (i32.const 0) (i32.const 0))
      (if (local.get $found)
        (then
          (i32.store (i32.const 4) (i32.const 2097216))
          (i32.store (i32.const 8) (i32.const 35)))
        (else
          (i32.store (i32.const 4) (i32.const 2097152))
          (i32.store (i32.const 8) (i32.const 34))))
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
