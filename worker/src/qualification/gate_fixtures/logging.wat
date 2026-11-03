;; Template: one logging.log call whose list holds COUNT field records, each naming NAME with
;; the value VALUE. Result: 0 = ok; 0xE0000000 | 0x100 | n is host-error case n.
(component
  (import "falcone:host/logging@0.1.0" (instance $logging
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (type $level0 (enum "debug" "info" "warn" "error")) (export "level" (type $level (eq $level0)))
    (type $field0 (record (field "name" string) (field "value" string))) (export "field" (type $field (eq $field0)))
    (export "log" (func (param "severity" $level) (param "message" string) (param "fields" (list $field)) (result (result (error $err)))))))
  (alias export $logging "log" (func $log))

  (core module $libc
    (memory (export "memory") 20)
    (data (i32.const 1024) "{{NAME}}")
    (data (i32.const 2048) "{{VALUE}}"))
  (core instance $libc (instantiate $libc))
  (core func $log_f (canon lower (func $log) (memory $libc "memory")))
  (core module $main
    (import "libc" "memory" (memory 20))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (local $i i32)
      (local $p i32)
      (loop $fill
        (local.set $p (i32.add (i32.const 65536) (i32.mul (local.get $i) (i32.const 16))))
        (i32.store (local.get $p) (i32.const 1024))
        (i32.store offset=4 (local.get $p) (i32.const {{NAME_LEN}}))
        (i32.store offset=8 (local.get $p) (i32.const 2048))
        (i32.store offset=12 (local.get $p) (i32.const {{VALUE_LEN}}))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $fill (i32.lt_u (local.get $i) (i32.const {{COUNT}}))))
      (call $log (i32.const 1) (i32.const 1024) (i32.const 0) (i32.const 65536) (i32.const {{COUNT}}) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 513)))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "log" (func $log_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
