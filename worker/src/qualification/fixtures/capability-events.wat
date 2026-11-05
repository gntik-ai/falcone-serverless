(component
  (import "falcone:host/events@0.1.0" (instance $events
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "publish" (func (param "destination" string) (param "payload" (list u8)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 4096 4096))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias export $events "publish" (func $publish))
  (core func $publish (canon lower (func $publish) (memory $memory)))
  (core module $guest
    (import "host" "memory" (memory 4096 4096))
    (import "host" "publish" (func $publish (param i32 i32 i32 i32 i32)))
    (data (i32.const 1024) "ordersother")
    (func (export "run") (param $mode i32) (result i32)
      (local $ptr i32) (local $len i32) (local $body i32)
      (local.set $ptr (i32.const 1024)) (local.set $len (i32.const 6))
      (if (i32.eq (local.get $mode) (i32.const 1)) (then
        (local.set $ptr (i32.const 1030)) (local.set $len (i32.const 5))))
      (if (i32.eq (local.get $mode) (i32.const 2)) (then
        (local.set $ptr (i32.const 4096)) (local.set $len (i32.const 201326592))))
      (if (i32.eq (local.get $mode) (i32.const 3)) (then
        (local.set $body (i32.const 201326592))))
      (call $publish (local.get $ptr) (local.get $len) (i32.const 4096) (local.get $body) (i32.const 0))
      (if (result i32) (i32.load8_u (i32.const 0))
        (then (i32.add (i32.const 1) (i32.load8_u (i32.const 1))))
        (else (i32.const 0)))))
  (core instance $host
    (export "memory" (memory $memory)) (export "publish" (func $publish)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func (export "run") (param "mode" u32) (result u32)
    (canon lift (core func $guest "run"))))
