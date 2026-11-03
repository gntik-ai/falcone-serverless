;; Calls the authority-free logging and progress sinks once each from the exported `run`.
;; Result: 0 = ok; 0xE0000000 | stage << 8 | n is host-error case n at stage (0x100 for
;; logging.log, 0x200 for progress.report).
(component
  (import "falcone:host/logging@0.1.0" (instance $logging
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (type $level0 (enum "debug" "info" "warn" "error")) (export "level" (type $level (eq $level0)))
    (type $field0 (record (field "name" string) (field "value" string))) (export "field" (type $field (eq $field0)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $err)))))))
  (import "falcone:host/progress@0.1.0" (instance $progress
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (export "report" (func (param "completed" u64) (param "total" u64) (param "message" string) (result (result (error $err)))))))
  (alias export $logging "log" (func $log))
  (alias export $progress "report" (func $report))

  (core module $libc
    (memory (export "memory") 1)
    (data (i32.const 1024) "x"))
  (core instance $libc (instantiate $libc))
  (core func $log_f (canon lower (func $log) (memory $libc "memory")))
  (core func $report_f (canon lower (func $report) (memory $libc "memory")))
  (core module $main
    (import "libc" "memory" (memory 1))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (import "host" "report" (func $report (param i64 i64 i32 i32 i32)))
    (func (export "run") (result i32)
      (call $log (i32.const 1) (i32.const 1024) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 513)))))))
      (call $report (i64.const 1) (i64.const 2) (i32.const 1024) (i32.const 1) (i32.const 520))
      (if (i32.load8_u (i32.const 520)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x200) (i32.load8_u (i32.const 521)))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "log" (func $log_f))
      (export "report" (func $report_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
