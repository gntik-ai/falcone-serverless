(component
  (import "falcone:host/logging@0.2.0" (instance $logging
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (type $level-type (enum "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level-type)))
    (type $field-type (record (field "name" string) (field "value" string)))
    (export "field" (type $field (eq $field-type)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 64))
  (core instance $memory (instantiate $memory))
  (alias export $logging "log" (func $log))
  (core func $log (canon lower (func $log) (memory $memory "memory")))
  (core instance $host
    (export "memory" (memory $memory "memory"))
    (export "log" (func $log)))

  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (import "host" "memory" (memory 64))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (export "memory" (memory 0))
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $n i32)
      ;; Log the received params as 1100 info lines (more than the 1000 the worker keeps), then
      ;; echo them: the maximum log volume, every line holding whatever the guest was given.
      (loop $again
        (call $log (i32.const 1) (local.get $ptr) (local.get $len) (i32.const 0) (i32.const 0) (i32.const 16))
        (local.set $n (i32.add (local.get $n) (i32.const 1)))
        (br_if $again (i32.lt_u (local.get $n) (i32.const 1100))))
      (i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (local.get $ptr))
      (i32.store (i32.const 8) (local.get $len))
      i32.const 0)
  )
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
)
