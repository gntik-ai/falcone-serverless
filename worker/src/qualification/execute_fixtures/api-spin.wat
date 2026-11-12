;; RUN-01.t3 fixture: a handler that reports progress once (1 of 1), then never returns on its
;; own. The progress report is the start signal a test polls through the update operation; only
;; the deadline or a cancel ends the guest, which makes it the blocked guest of the saturation
;; and cancel cases.
(component
  (import "falcone:host/progress@0.2.0" (instance $progress
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "report" (func (param "completed" u64) (param "total" u64) (param "message" string) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 64))
  (core instance $memory (instantiate $memory))
  (alias export $progress "report" (func $report))
  (core func $report (canon lower (func $report) (memory $memory "memory")))
  (core instance $host
    (export "memory" (memory $memory "memory"))
    (export "report" (func $report)))
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    (import "host" "memory" (memory 64))
    (import "host" "report" (func $report (param i64 i64 i32 i32 i32)))
    (export "memory" (memory 0))
    (global $heap (mut i32) (i32.const 64))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (call $report (i64.const 1) (i64.const 1) (i32.const 0) (i32.const 0) (i32.const 16))
      (loop $forever (br $forever))
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
