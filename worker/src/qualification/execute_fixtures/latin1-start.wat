(component
  (import "falcone:host/events@0.2.0" (instance $events
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "publish" (func (param "destination" string) (param "payload" (list u8)) (result (result (error $error)))))))
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
  (alias export $events "publish" (func $publish))
  (alias export $logging "log" (func $log))
  (core func $publish (canon lower (func $publish) (memory $memory "memory")))
  (core func $log (canon lower (func $log) (memory $memory "memory")))
  (core instance $host
    (export "memory" (memory $memory "memory"))
    (export "publish" (func $publish))
    (export "log" (func $log)))

  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (import "host" "memory" (memory 64))
    (import "host" "publish" (func $publish (param i32 i32 i32 i32 i32)))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (data (i32.const 32) "orders")
    (export "memory" (memory 0))
    (global $heap (mut i32) (i32.const 64))
    (func $start (call $publish (i32.const 32) (i32.const 6) (i32.const 128) (i32.const 0) (i32.const 16)))
    (start $start)
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $mode i32) (local $size i32)
      local.get $ptr i32.load8_u local.set $mode
      local.get $ptr i32.load offset=4 local.set $size
      ;; Modes 5..8 perform a mutation, 9..10 perform a non-mutating log.
      (if (i32.and (i32.ge_u (local.get $mode) (i32.const 5)) (i32.le_u (local.get $mode) (i32.const 8))) (then
        (call $publish (i32.const 32) (i32.const 6) (i32.const 128) (i32.const 0) (i32.const 16))))
      (if (i32.ge_u (local.get $mode) (i32.const 9)) (then
        (call $log (i32.const 0) (i32.const 128) (i32.const 0) (i32.const 128) (i32.const 0) (i32.const 16))))
      (if (i32.or (i32.eq (local.get $mode) (i32.const 3)) (i32.eq (local.get $mode) (i32.const 11))) (then (loop br 0)))
      (if (i32.or (i32.eq (local.get $mode) (i32.const 4)) (i32.eq (local.get $mode) (i32.const 7))) (then unreachable))
      ;; Fill guest output independently of the small parameter envelope.
      (memory.fill (i32.const 128) (i32.const 120) (local.get $size))
      (i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (i32.const 128))
      (i32.store (i32.const 8) (local.get $size))
      (if (i32.or (i32.or (i32.eq (local.get $mode) (i32.const 1)) (i32.eq (local.get $mode) (i32.const 2)))
                 (i32.or (i32.eq (local.get $mode) (i32.const 6)) (i32.eq (local.get $mode) (i32.const 10)))) (then
        (i32.store (i32.const 0) (i32.const 1))
        (i32.store (i32.const 4) (i32.const 1))
        (i32.store (i32.const 8) (i32.const 128))
        (i32.store (i32.const 12) (local.get $size))))
      (if (i32.eq (local.get $mode) (i32.const 1)) (then (i32.store (i32.const 4) (i32.const 0))))
      i32.const 0)
  )
  (core instance $guest (instantiate $guest (with "host" (instance $host))
  ))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=latin1+utf16))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
)

