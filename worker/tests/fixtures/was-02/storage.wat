;; Template: opens {{KEY}} and reads it (OP=0) or writes DATA_LEN zero bytes (OP=1).
;; Result: 0 = ok; a read returns the byte length; an http response returns
;; (status << 16) | body length; 0xE0000000 | stage << 8 | n is host-error case n at stage.
(component
  (import "falcone:host/storage@0.1.0" (instance $storage
    (export "object" (type $object (sub resource)))
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (export "open-object" (func (param "key" string) (result (result (own $object) (error $err)))))
    (export "read-object" (func (param "handle" (borrow $object)) (result (result (list u8) (error $err)))))
    (export "write-object" (func (param "handle" (borrow $object)) (param "data" (list u8)) (result (result (error $err)))))))
  (alias export $storage "open-object" (func $open))
  (alias export $storage "read-object" (func $read))
  (alias export $storage "write-object" (func $write))
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
    (data (i32.const 1024) "{{KEY}}")
)
  (core instance $libc (instantiate $libc))
  (core func $open_f (canon lower (func $open) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core func $read_f (canon lower (func $read) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core func $write_f (canon lower (func $write) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory 20))
    (import "host" "open" (func $open (param i32 i32 i32)))
    (import "host" "read" (func $read (param i32 i32)))
    (import "host" "write" (func $write (param i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (local $handle i32)
      (call $open (i32.const 1024) (i32.const {{KEY_LEN}}) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 516)))))))
      (local.set $handle (i32.load (i32.const 516)))
      (if (i32.eqz (i32.const {{OP}}))
        (then
          (call $read (local.get $handle) (i32.const 528))
          (if (i32.load8_u (i32.const 528)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x200) (i32.load8_u (i32.const 532)))))))
          (return (i32.load (i32.const 536)))))
      (call $write (local.get $handle) (i32.const 65536) (i32.const {{DATA_LEN}}) (i32.const 528))
      (if (i32.load8_u (i32.const 528)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x200) (i32.load8_u (i32.const 529)))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "open" (func $open_f))
      (export "read" (func $read_f))
      (export "write" (func $write_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
