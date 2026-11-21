(component
  (import "wasi:io/error@0.2.0" (instance $errors (export "error" (type (sub resource)))))
  (alias export $errors "error" (type $error))
  (import "wasi:io/streams@0.2.0" (instance $streams
    (alias outer 1 $error (type $outer-error))
    (export "error" (type $error (eq $outer-error)))
    (export "output-stream" (type $output (sub resource)))
    (type $stream-error-type (variant (case "last-operation-failed" (own $error)) (case "closed")))
    (export "stream-error" (type $stream-error (eq $stream-error-type)))
    (export "[method]output-stream.write" (func (param "self" (borrow $output)) (param "contents" (list u8)) (result (result (error $stream-error)))))
    (export "[method]output-stream.blocking-write-and-flush" (func (param "self" (borrow $output)) (param "contents" (list u8)) (result (result (error $stream-error)))))
    (export "[method]output-stream.flush" (func (param "self" (borrow $output)) (result (result (error $stream-error)))))
    (export "[method]output-stream.blocking-flush" (func (param "self" (borrow $output)) (result (result (error $stream-error)))))
    (export "[method]output-stream.check-write" (func (param "self" (borrow $output)) (result (result u64 (error $stream-error)))))))
  (alias export $streams "output-stream" (type $output))
  (import "wasi:cli/stdout@0.2.0" (instance $stdout
    (alias outer 1 $output (type $outer-output))
    (export "output-stream" (type $output (eq $outer-output)))
    (export "get-stdout" (func (result (own $output))))))
  (core module $mem (memory (export "memory") 2))
  (core instance $mem (instantiate $mem))
  (alias export $stdout "get-stdout" (func $get))
  (alias export $streams "[method]output-stream.write" (func $write))
  (alias export $streams "[method]output-stream.blocking-write-and-flush" (func $blocking))
  (alias export $streams "[method]output-stream.flush" (func $flush))
  (alias export $streams "[method]output-stream.blocking-flush" (func $blocking-flush))
  (alias export $streams "[method]output-stream.check-write" (func $check))
  (core func $get (canon lower (func $get)))
  (core func $write (canon lower (func $write) (memory $mem "memory")))
  (core func $blocking (canon lower (func $blocking) (memory $mem "memory")))
  (core func $flush (canon lower (func $flush) (memory $mem "memory")))
  (core func $blocking-flush (canon lower (func $blocking-flush) (memory $mem "memory")))
  (core func $check (canon lower (func $check) (memory $mem "memory")))
  (core func $drop (canon resource.drop $output))
  (core module $guest
    (import "host" "memory" (memory 2))
    (import "host" "get" (func $get (result i32)))
    (import "host" "write" (func $write (param i32 i32 i32 i32)))
    (import "host" "blocking" (func $blocking (param i32 i32 i32 i32)))
    (import "host" "flush" (func $flush (param i32 i32)))
    (import "host" "blocking-flush" (func $blocking-flush (param i32 i32)))
    (import "host" "check" (func $check (param i32 i32)))
    (import "host" "drop" (func $drop (param i32)))
    (func (export "write") (param $len i32) (param $blocking-mode i32) (result i64)
      (local $handle i32)
      call $get local.set $handle
      local.get $blocking-mode
      if
        local.get $handle i32.const 1024 local.get $len i32.const 0 call $blocking
      else
        local.get $handle i32.const 1024 local.get $len i32.const 0 call $write
      end
      i32.const 0 i32.load if unreachable end
      local.get $handle i32.const 0 call $flush
      i32.const 0 i32.load if unreachable end
      local.get $handle i32.const 0 call $blocking-flush
      i32.const 0 i32.load if unreachable end
      local.get $handle i32.const 0 call $check
      i32.const 0 i32.load if unreachable end
      local.get $handle call $drop
      i32.const 8 i64.load))
  (core instance $guest (instantiate $guest (with "host" (instance
    (export "memory" (memory $mem "memory"))
    (export "get" (func $get)) (export "write" (func $write))
    (export "blocking" (func $blocking)) (export "flush" (func $flush))
    (export "blocking-flush" (func $blocking-flush))
    (export "check" (func $check)) (export "drop" (func $drop))))))
  (func (export "write") (param "len" u32) (param "blocking" u32) (result u64)
    (canon lift (core func $guest "write"))))
