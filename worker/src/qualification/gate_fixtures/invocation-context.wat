;; Reads each verified invocation-context identifier. Exports key, attempt and idempotency; each
;; returns the byte length of the string the host wrote into guest memory.
(component
  (import "falcone:host/invocation-context@0.1.0" (instance $ctx
    (export "invocation-key" (func (result string)))
    (export "attempt-id" (func (result string)))
    (export "idempotency-key" (func (result string)))))
  (alias export $ctx "invocation-key" (func $key))
  (alias export $ctx "attempt-id" (func $attempt))
  (alias export $ctx "idempotency-key" (func $idem))
  (core module $libc
    (memory (export "memory") 40)
    (global $bump (mut i32) (i32.const 4096))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and
        (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get 2))))
      (global.set $bump (i32.add (local.get $p) (local.get 3)))
      (local.get $p)))
  (core instance $libc (instantiate $libc))
  (core func $key_f (canon lower (func $key) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core func $attempt_f (canon lower (func $attempt) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core func $idem_f (canon lower (func $idem) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory 40))
    (import "host" "key" (func $key (param i32)))
    (import "host" "attempt" (func $attempt (param i32)))
    (import "host" "idem" (func $idem (param i32)))
    (func (export "key") (result i32)
      (call $key (i32.const 512))
      (i32.load (i32.const 516)))
    (func (export "attempt") (result i32)
      (call $attempt (i32.const 512))
      (i32.load (i32.const 516)))
    (func (export "idempotency") (result i32)
      (call $idem (i32.const 512))
      (i32.load (i32.const 516))))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "key" (func $key_f))
      (export "attempt" (func $attempt_f))
      (export "idem" (func $idem_f))))))
  (func (export "key") (result u32) (canon lift (core func $main "key")))
  (func (export "attempt") (result u32) (canon lift (core func $main "attempt")))
  (func (export "idempotency") (result u32) (canon lift (core func $main "idempotency")))
)
