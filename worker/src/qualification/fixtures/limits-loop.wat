(component
  (core module $guest
    (func (export "run") (result i32) (loop br 0) i32.const 42))
  (core instance $guest (instantiate $guest))
  (func (export "run") (result s32) (canon lift (core func $guest "run"))))
