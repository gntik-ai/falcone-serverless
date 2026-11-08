;; Core start function calls logging.log before any export is used.
;; The handler instance is empty (no handle): the gate must reject it before this runs.
(component
  (import "falcone:host/logging@0.2.0" (instance $logging
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (type $level-type (enum "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level-type)))
    (type $field-type (record (field "name" string) (field "value" string)))
    (export "field" (type $field (eq $field-type)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 32))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias export $logging "log" (func $log))
  (core func $log (canon lower (func $log) (memory $memory)))
  (core module $guest
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (func $start (call $log (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 64) (i32.const 0) (i32.const 128)))
    (start $start)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "handle") (param i32 i32) (result i32) i32.const 16))
  (core instance $host (export "log" (func $log)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (instance $handler)
  (export "falcone:host/handler@0.2.0" (instance $handler)))
