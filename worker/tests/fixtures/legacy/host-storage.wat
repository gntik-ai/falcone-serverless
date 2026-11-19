;; Offline WIT-host probe, not a substitute for the trusted-image-built JS wrapper.
;; The handler receives a raw storage key. Only host-error.denied becomes handler.failed;
;; any other host error traps, and an authorized read returns the JSON literal true.
(component
  (import "falcone:host/storage@0.2.0" (instance $storage
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "object" (type $object (sub resource)))
    (export "open-object" (func (param "key" string) (result (result (own $object) (error $error)))))
    (export "read-object" (func (param "handle" (borrow $object)) (result (result (list u8) (error $error)))))))
  (core module $memory
    (memory (export "memory") 64)
    (global $heap (mut i32) (i32.const 65536))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      (local.set $ptr (global.get $heap))
      (global.set $heap (i32.add (global.get $heap) (local.get $size)))
      (local.get $ptr)))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias core export $memory "realloc" (core func $realloc))
  (alias export $storage "open-object" (func $open))
  (alias export $storage "read-object" (func $read))
  (core func $open (canon lower (func $open) (memory $memory)))
  (core func $read (canon lower (func $read) (memory $memory) (realloc $realloc)))
  (core module $guest
    (import "host" "memory" (memory 64))
    (import "host" "open" (func $open (param i32 i32 i32)))
    (import "host" "read" (func $read (param i32 i32)))
    (data (i32.const 1024) "host denied")
    (data (i32.const 1040) "true")
    (func $denied (result i32)
      ;; Canonical result<list<u8>, handler-error.failed(string)>.
      (i32.store (i32.const 0) (i32.const 1))
      (i32.store (i32.const 4) (i32.const 1))
      (i32.store (i32.const 8) (i32.const 1024))
      (i32.store (i32.const 12) (i32.const 11))
      (i32.const 0))
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (call $open (local.get $ptr) (local.get $len) (i32.const 0))
      (if (i32.load8_u (i32.const 0)) (then
        (if (i32.load8_u (i32.const 4)) (then unreachable))
        (return (call $denied))))
      (call $read (i32.load (i32.const 4)) (i32.const 0))
      (if (i32.load8_u (i32.const 0)) (then
        (if (i32.load8_u (i32.const 4)) (then unreachable))
        (return (call $denied))))
      (i32.store (i32.const 0) (i32.const 0))
      (i32.store (i32.const 4) (i32.const 1040))
      (i32.store (i32.const 8) (i32.const 4))
      (i32.const 0)))
  (core instance $host
    (export "memory" (memory $memory)) (export "open" (func $open)) (export "read" (func $read)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (type $bytes (list u8))
  (type $handler-error (variant (case "invalid-input" string) (case "failed" string)))
  (func $handle (param "params" $bytes) (result (result $bytes (error $handler-error)))
    (canon lift (core func $guest "handle") (memory $memory) (realloc $realloc) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $handler-error)) (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))
