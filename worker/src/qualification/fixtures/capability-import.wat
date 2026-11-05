(component
  (import "IMPORT" (instance))
  (import "falcone:host/logging@0.1.0" (instance $logging
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (type $level-type (enum "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level-type)))
    (type $field-type (record (field "name" string) (field "value" string)))
    (export "field" (type $field (eq $field-type)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 1 1))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias export $logging "log" (func $log))
  (core func $log (canon lower (func $log) (memory $memory)))
  (core module $guest
    (import "host" "memory" (memory 1 1))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (data (i32.const 1024) "start-marker")
    (func $start
      (call $log (i32.const 0) (i32.const 1024) (i32.const 12) (i32.const 0) (i32.const 0) (i32.const 0)))
    (start $start))
  (core instance $host (export "memory" (memory $memory)) (export "log" (func $log)))
  (core instance (instantiate $guest (with "host" (instance $host)))))
