(component
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (memory (export "memory") 64)
    ;; The needle sits below the scanned range, with the result template.
    (data (i32.const 16) "marker")
    (data (i32.const 32) "{\"hits\":0}")
    (global $heap (mut i32) (i32.const 8192))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    ;; Counts the occurrences of the needle in the whole linear memory above 4096, the range
    ;; that holds the params and the guest heap: whatever the host wrote into this guest is there.
    (func $scan (result i32)
      (local $i i32) (local $limit i32) (local $hits i32)
      (local.set $i (i32.const 4096))
      (local.set $limit (i32.sub (i32.shl (memory.size) (i32.const 16)) (i32.const 6)))
      (block $done
        (loop $next
          (br_if $done (i32.ge_u (local.get $i) (local.get $limit)))
          (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 109))
            (then
              (if (i32.and
                    (i32.eq (i32.load (local.get $i)) (i32.load (i32.const 16)))
                    (i32.eq (i32.load16_u offset=4 (local.get $i)) (i32.load16_u offset=4 (i32.const 16))))
                (then (local.set $hits (i32.add (local.get $hits) (i32.const 1)))))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $next)))
      (local.get $hits))
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $hits i32)
      (local.set $hits (call $scan))
      (if (i32.gt_u (local.get $hits) (i32.const 9)) (then (local.set $hits (i32.const 9))))
      (i32.store8 (i32.const 40) (i32.add (i32.const 48) (local.get $hits)))
      (i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (i32.const 32))
      (i32.store (i32.const 8) (i32.const 10))
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
