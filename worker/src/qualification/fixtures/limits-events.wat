(component
  (import "falcone:host/events@0.1.0" (instance $events
    (type $error-type (variant (case "denied") (case "limit-exceeded") (case "timeout") (case "cancelled")))
    (export "host-error" (type $error (eq $error-type)))
    (export "publish" (func (param "destination" string) (param "payload" (list u8)) (result (result (error $error)))))))
  (core module $memory (memory (export "memory") 2 4))
  (core instance $memory (instantiate $memory))
  (alias core export $memory "memory" (core memory $memory))
  (alias export $events "publish" (func $publish))
  (core func $publish (canon lower (func $publish) (memory $memory)))
  (core module $guest
    (import "host" "memory" (memory 2 4))
    (import "host" "publish" (func $publish (param i32 i32 i32 i32 i32)))
    (data (i32.const 1024) "ordersother")
    (func (export "run") (param $mode i32) (result i32)
      (call $publish (i32.const 1024) (i32.const 6) (i32.const 4096) (local.get $mode) (i32.const 0))
      (if (result i32) (i32.load8_u (i32.const 0))
        (then (i32.add (i32.const 1) (i32.load8_u (i32.const 1))))
        (else (i32.const 0)))))
  (core instance $host
    (export "memory" (memory $memory)) (export "publish" (func $publish)))
  (core instance $guest (instantiate $guest (with "host" (instance $host))))
  (func (export "run") (param "mode" u32) (result u32)
    (canon lift (core func $guest "run"))))
