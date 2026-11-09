(component
  (import "falcone:host/logging@0.2.0" (instance $logging
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (type $level-type (enum "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $level-type)))
    (type $field-type (record (field "name" string) (field "value" string)))
    (export "field" (type $field (eq $field-type)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 4096 4096))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias export $logging "log" (func $log))
  (core func $log (canon lower (func $log) (memory $memory)))
  (core module $guest
    (import "host" "memory" (memory 4096 4096))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (func (export "run") (param $mode i32) (result i32)
      (local $count i32)
      (local.set $count (i32.const 1))
      (i32.store (i32.const 4096) (i32.const 8192))
      (i32.store (i32.const 4100) (i32.const 0))
      (if (i32.eq (local.get $mode) (i32.const 1)) (then
        (i32.store (i32.const 4100) (i32.const 201326592))))
      (if (i32.eq (local.get $mode) (i32.const 2)) (then
        (local.set $count (i32.const 1048577))
        ;; Poison the first element: any eager element read traps instead of
        ;; returning limit-exceeded. The remaining guest pages stay untouched.
        (i32.store (i32.const 4096) (i32.const -1))
        (i32.store (i32.const 4100) (i32.const 1))))
      (if (i32.eq (local.get $mode) (i32.const 3)) (then
        (local.set $count (i32.const 1048577))))
      (if (i32.eq (local.get $mode) (i32.const 4)) (then
        (i32.store (i32.const 4100) (i32.const 1048559))))
      (if (i32.eq (local.get $mode) (i32.const 5)) (then
        (i32.store (i32.const 4100) (i32.const 1048560))))
      (if (i32.eq (local.get $mode) (i32.const 6)) (then
        (local.set $count (i32.const 2))
        (i32.store (i32.const 4100) (i32.const 1048559))
        (i32.store (i32.const 4112) (i32.const -1))
        (i32.store (i32.const 4116) (i32.const 1))))
      (call $log (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 4096) (local.get $count) (i32.const 0))
      (if (result i32) (i32.load8_u (i32.const 0))
        (then (i32.add (i32.const 1) (i32.load8_u (i32.const 1))))
        (else (i32.const 0)))))
  (core instance $host (export "memory" (memory $memory)) (export "log" (func $log)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func (export "run") (param "mode" u32) (result u32)
    (canon lift (core func $guest "run"))))
