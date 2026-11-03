;; Template: publishes PAYLOAD_LEN zero bytes to the topic {{TOPIC}}.
;; Result: 0 = ok; a read returns the byte length; an http response returns
;; (status << 16) | body length; 0xE0000000 | stage << 8 | n is host-error case n at stage.
(component
  (import "falcone:host/events@0.1.0" (instance $events
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (export "publish" (func (param "destination" string) (param "payload" (list u8)) (result (result (error $err)))))))
  (alias export $events "publish" (func $publish))
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
    (data (i32.const 1024) "{{TOPIC}}")
)
  (core instance $libc (instantiate $libc))
  (core func $publish_f (canon lower (func $publish) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory 20))
    (import "host" "publish" (func $publish (param i32 i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (call $publish (i32.const 1024) (i32.const {{TOPIC_LEN}}) (i32.const 65536) (i32.const {{PAYLOAD_LEN}}) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 513)))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "publish" (func $publish_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
