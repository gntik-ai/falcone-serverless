;; Template: starts the workflow {{TARGET}}; SIGNAL=1 then signals the handle, SIGNAL=2 signals a handle index the host never issued.
;; Result: 0 = ok; a read returns the byte length; an http response returns
;; (status << 16) | body length; 0xE0000000 | stage << 8 | n is host-error case n at stage.
(component
  (import "falcone:host/workflows@0.1.0" (instance $workflows
    (export "execution" (type $execution (sub resource)))
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (export "start" (func (param "workflow" string) (param "input" (list u8)) (result (result (own $execution) (error $err)))))
    (export "signal" (func (param "handle" (borrow $execution)) (param "name" string) (param "payload" (list u8)) (result (result (error $err)))))))
  (alias export $workflows "start" (func $start))
  (alias export $workflows "signal" (func $signal))
  (core module $libc
    (memory (export "memory") 20)
    (global $bump (mut i32) (i32.const 65536))
    ;; Bump allocator for lists and strings that the host writes into guest memory.
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and
        (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get 2))))
      (global.set $bump (i32.add (local.get $p) (local.get 3)))
      (local.get $p))
    (data (i32.const 1024) "{{TARGET}}")
    (data (i32.const 2048) "go")
)
  (core instance $libc (instantiate $libc))
  (core func $start_f (canon lower (func $start) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core func $signal_f (canon lower (func $signal) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory 20))
    (import "host" "start" (func $start (param i32 i32 i32 i32 i32)))
    (import "host" "signal" (func $signal (param i32 i32 i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (local $handle i32)
      (call $start (i32.const 1024) (i32.const {{TARGET_LEN}}) (i32.const 65536) (i32.const 4) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 516)))))))
      (local.set $handle (i32.load (i32.const 516)))
      (if (i32.eq (i32.const {{SIGNAL}}) (i32.const 2)) (then (local.set $handle (i32.const 99))))
      (if (i32.ne (i32.const {{SIGNAL}}) (i32.const 0))
        (then
          (call $signal (local.get $handle) (i32.const 2048) (i32.const 2) (i32.const 65536) (i32.const 4) (i32.const 528))
          (if (i32.load8_u (i32.const 528)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x200) (i32.load8_u (i32.const 529)))))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "start" (func $start_f))
      (export "signal" (func $signal_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
