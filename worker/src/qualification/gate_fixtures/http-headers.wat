;; Template: GET {{URL}} with COUNT header records that all name the same NAME_LEN bytes at 1024
;; and the same VALUE_LEN bytes at 1048576, so a small list can alias one large guest string.
;; The memory has PAGES pages. Result: 0 = ok; 0xE0000000 | 0x100 | n is host-error case n.
(component
  (import "falcone:host/http@0.1.0" (instance $http
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (type $header0 (record (field "name" string) (field "value" string))) (export "header" (type $header (eq $header0)))
    (type $response0 (record (field "status" u16) (field "headers" (list $header)) (field "body" (list u8)))) (export "response" (type $response (eq $response0)))
    (export "request" (func (param "url" string) (param "method" string) (param "headers" (list $header)) (param "body" (list u8)) (result (result $response (error $err)))))))
  (alias export $http "request" (func $request))
  (core module $libc
    (memory (export "memory") {{PAGES}})
    (global $bump (mut i32) (i32.const 3145728))
    ;; Bump allocator for lists and strings that the host writes into guest memory.
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      (local.set $p (i32.and
        (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get 2))))
      (global.set $bump (i32.add (local.get $p) (local.get 3)))
      (local.get $p))
    (data (i32.const 256) "{{URL}}")
    (data (i32.const 1024) "x")
    (data (i32.const 4000) "GET"))
  (core instance $libc (instantiate $libc))
  (core func $request_f (canon lower (func $request) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory {{PAGES}}))
    (import "host" "request" (func $request (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (local $i i32)
      (local $p i32)
      (if (i32.const {{COUNT}}) (then
        (loop $fill
          (local.set $p (i32.add (i32.const 65536) (i32.mul (local.get $i) (i32.const 16))))
          (i32.store (local.get $p) (i32.const 1024))
          (i32.store offset=4 (local.get $p) (i32.const {{NAME_LEN}}))
          (i32.store offset=8 (local.get $p) (i32.const 1048576))
          (i32.store offset=12 (local.get $p) (i32.const {{VALUE_LEN}}))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br_if $fill (i32.lt_u (local.get $i) (i32.const {{COUNT}}))))))
      (call $request (i32.const 256) (i32.const {{URL_LEN}}) (i32.const 4000) (i32.const 3) (i32.const 65536) (i32.const {{COUNT}}) (i32.const 0) (i32.const 0) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 516)))))))
      (i32.const 0)))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "request" (func $request_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
