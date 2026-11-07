(component
  (core module $guest
    (memory (export "memory") 2 16)
    ;; The guest handles the denied growth and returns successfully.
    (func (export "run") (param $pages i32) (result i32)
      (memory.grow (local.get $pages))))
  (core instance $guest (instantiate $guest))
  (func (export "run") (param "pages" u32) (result s32)
    (canon lift (core func $guest "run"))))
