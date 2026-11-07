;; A valid control fixture: returns 42 at once.
(component
  (core module $guest
    (func (export "run") (result i32) i32.const 42))
  (core instance $guest (instantiate $guest))
  (func (export "run") (result u32) (canon lift (core func $guest "run"))))
