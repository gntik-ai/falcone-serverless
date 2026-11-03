;; Imports falcone:host/storage without being granted it; also the control that proves the start marker works when a grant exists.
;; The core start function logs "start" through falcone:host/logging. The host records that
;; message, so a recorded "start" proves the guest ran and a missing one proves it never did.
(component
  (import "falcone:host/storage@0.1.0" (instance))
  (import "falcone:host/logging@0.1.0" (instance $logging
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (type $level0 (enum "debug" "info" "warn" "error")) (export "level" (type $level (eq $level0)))
    (type $field0 (record (field "name" string) (field "value" string))) (export "field" (type $field (eq $field0)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $err)))))))
  (alias export $logging "log" (func $log))

  (core module $libc
    (memory (export "memory") 1)
    (data (i32.const 1024) "start"))
  (core instance $libc (instantiate $libc))
  (core func $log_f (canon lower (func $log) (memory $libc "memory")))
  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (func $start
      (call $log (i32.const 1) (i32.const 1024) (i32.const 5) (i32.const 0) (i32.const 0) (i32.const 512)))
    (start $start))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance (export "log" (func $log_f))))))
)
