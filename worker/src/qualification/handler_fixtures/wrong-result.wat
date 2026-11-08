;; handle returns u32 instead of result<list<u8>, handler-error>.
(component
  (core module $guest
    (memory (export "memory") 32)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "handle") (param i32 i32) (result i32) i32.const 0))
  (core instance $guest (instantiate $guest))
  (func $handle (param "params" (list u8)) (result u32)
    (canon lift (core func $guest "handle") (memory $guest "memory") (realloc (func $guest "realloc"))))
  (instance $handler (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler)))
