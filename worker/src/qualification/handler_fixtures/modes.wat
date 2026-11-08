;; The first params byte selects the behaviour. The guest imports only logging, whose
;; `log` call is the host call that cancellation and deadline cases interleave with.
;;   'e' echo params         's' return u32-LE(params[1..5]) bytes   'i' invalid-input("bad input")
;;   'f' failed("boom")      'g' failed(1000 x 'x')                   'l' loop forever
;;   't' trap                'k' log, then return ok("done")         'm' log, then failed("boom")
;;   'n' log, then invalid-input("bad input")
;;   'G' failed(string of u32-LE(params[1..5]) NUL bytes)
;;   'F' log with u32-LE(params[1..5]) empty fields, then return ok("done")
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
    (import "host" "memory" (memory 32))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (data (i32.const 2048) "bad input")
    (data (i32.const 2080) "boom")
    (data (i32.const 2096) "done")
    (global $heap (mut i32) (i32.const 8192))
    (func (export "realloc") (param $old i32) (param $old_size i32) (param $align i32) (param $new_size i32) (result i32)
      (local $ptr i32)
      (local.set $ptr (global.get $heap))
      (global.set $heap (i32.add (global.get $heap) (local.get $new_size)))
      (local.get $ptr))
    (func $ok (param $ptr i32) (param $len i32) (result i32)
      (i32.store8 (i32.const 16) (i32.const 0))
      (i32.store (i32.const 20) (local.get $ptr))
      (i32.store (i32.const 24) (local.get $len))
      (i32.const 16))
    ;; err(case, string): case 0 is invalid-input, case 1 is failed.
    (func $err (param $case i32) (param $ptr i32) (param $len i32) (result i32)
      (i32.store8 (i32.const 16) (i32.const 1))
      (i32.store8 (i32.const 20) (local.get $case))
      (i32.store (i32.const 24) (local.get $ptr))
      (i32.store (i32.const 28) (local.get $len))
      (i32.const 16))
    (func $log_once
      (call $log (i32.const 0) (i32.const 2096) (i32.const 4) (i32.const 4096) (i32.const 0) (i32.const 64)))
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $mode i32)
      (local.set $mode (i32.load8_u (local.get $ptr)))
      (if (i32.eq (local.get $mode) (i32.const 101)) (then (return (call $ok (local.get $ptr) (local.get $len)))))
      (if (i32.eq (local.get $mode) (i32.const 115)) (then
        (return (call $ok (i32.const 8192) (i32.load (i32.add (local.get $ptr) (i32.const 1)))))))
      (if (i32.eq (local.get $mode) (i32.const 105)) (then
        (return (call $err (i32.const 0) (i32.const 2048) (i32.const 9)))))
      (if (i32.eq (local.get $mode) (i32.const 102)) (then
        (return (call $err (i32.const 1) (i32.const 2080) (i32.const 4)))))
      (if (i32.eq (local.get $mode) (i32.const 103)) (then
        (memory.fill (i32.const 16384) (i32.const 120) (i32.const 1000))
        (return (call $err (i32.const 1) (i32.const 16384) (i32.const 1000)))))
      (if (i32.eq (local.get $mode) (i32.const 71)) (then
        (return (call $err (i32.const 1) (i32.const 16384) (i32.load (i32.add (local.get $ptr) (i32.const 1)))))))
      (if (i32.eq (local.get $mode) (i32.const 70)) (then
        (call $log (i32.const 0) (i32.const 2096) (i32.const 4) (i32.const 65536)
          (i32.load (i32.add (local.get $ptr) (i32.const 1))) (i32.const 64))
        (return (call $ok (i32.const 2096) (i32.const 4)))))
      (if (i32.eq (local.get $mode) (i32.const 108)) (then (loop $forever (br $forever))))
      (if (i32.eq (local.get $mode) (i32.const 116)) (then unreachable))
      (if (i32.eq (local.get $mode) (i32.const 107)) (then
        (call $log_once)
        (return (call $ok (i32.const 2096) (i32.const 4)))))
      (if (i32.eq (local.get $mode) (i32.const 109)) (then
        (call $log_once)
        (return (call $err (i32.const 1) (i32.const 2080) (i32.const 4)))))
      (if (i32.eq (local.get $mode) (i32.const 110)) (then
        (call $log_once)
        (return (call $err (i32.const 0) (i32.const 2048) (i32.const 9)))))
      (call $err (i32.const 0) (i32.const 2048) (i32.const 9))))
  (core instance $host (export "memory" (memory $memory)) (export "log" (func $log)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $reply (result (list u8) (error $error)))
  (func $handle (param "params" (list u8)) (result $reply)
    (canon lift (core func $guest "handle") (memory $memory) (realloc (func $guest "realloc"))))
  (instance $handler (export "handler-error" (type $error)) (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))
