;; Template: GET {{URL}} with no headers or body.
;; Result: 0 = ok; a read returns the byte length; an http response returns
;; (status << 16) | body length; 0xE0000000 | stage << 8 | n is host-error case n at stage.
(component
  (import "falcone:host/http@0.1.0" (instance $http
    (type $err0 (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled"))) (export "host-error" (type $err (eq $err0)))
    (type $header0 (record (field "name" string) (field "value" string))) (export "header" (type $header (eq $header0)))
    (type $response0 (record (field "status" u16) (field "headers" (list $header)) (field "body" (list u8)))) (export "response" (type $response (eq $response0)))
    (export "request" (func (param "url" string) (param "method" string) (param "headers" (list $header)) (param "body" (list u8)) (result (result $response (error $err)))))))
  (alias export $http "request" (func $request))
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
    (data (i32.const 1024) "{{URL}}")
    (data (i32.const 4000) "GET")
)
  (core instance $libc (instantiate $libc))
  (core func $request_f (canon lower (func $request) (memory $libc "memory") (realloc (func $libc "realloc"))))
  (core module $main
    (import "libc" "memory" (memory 20))
    (import "host" "request" (func $request (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
    (func (export "run") (result i32)
      (call $request (i32.const 1024) (i32.const {{URL_LEN}}) (i32.const 4000) (i32.const 3) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 512))
      (if (i32.load8_u (i32.const 512)) (then (return (i32.or (i32.const 0xE0000000) (i32.or (i32.const 0x100) (i32.load8_u (i32.const 516)))))))
      (i32.or (i32.shl (i32.load16_u (i32.const 516)) (i32.const 16)) (i32.and (i32.load (i32.const 532)) (i32.const 0xFFFF)))))
  (core instance $main (instantiate $main
    (with "libc" (instance $libc))
    (with "host" (instance
      (export "request" (func $request_f))))))
  (func (export "run") (result u32) (canon lift (core func $main "run")))
)
