(component
  (import "falcone:host/storage@0.2.0" (instance $storage
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "object" (type $object (sub resource)))
    (export "open-object" (func (param "key" string) (result (result (own $object) (error $error)))))))
  (alias export $storage "object" (type $object))
  (core module $memory (memory (export "memory") 64))
  (core instance $memory (instantiate $memory))
  (alias export $storage "open-object" (func $open))
  (core func $open (canon lower (func $open) (memory $memory "memory")))
  (core func $drop (canon resource.drop $object))
  (core instance $host
    (export "memory" (memory $memory "memory"))
    (export "open" (func $open))
    (export "drop" (func $drop)))
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (import "host" "memory" (memory 64))
    (import "host" "open" (func $open (param i32 i32 i32)))
    (import "host" "drop" (func $drop (param i32)))
    (data (i32.const 2097152) "{\"statusCode\":200,\"body\":\"ok\"}")
    (data (i32.const 32) "objects/a")
    (export "memory" (memory 0))
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    ;; Two rounds of open-object. A granted key yields a handle, which is dropped: four
    ;; guest-to-host entries. A refused key yields an error and no handle: two entries.
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $round i32)
      (loop $again
        (call $open (i32.const 32) (i32.const 9) (i32.const 256))
        (if (i32.eqz (i32.load8_u (i32.const 256)))
          (then (call $drop (i32.load (i32.const 260)))))
        (local.set $round (i32.add (local.get $round) (i32.const 1)))
        (br_if $again (i32.lt_u (local.get $round) (i32.const 2))))
      i32.const 0 i32.const 0 i32.store
      i32.const 4 i32.const 2097152 i32.store
      i32.const 8 i32.const 30 i32.store
      i32.const 0
    ))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
)
