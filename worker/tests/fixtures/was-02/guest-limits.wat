(component
  (core module $guest
    (memory 1)
    (func (export "run") (param $mode i32) (result i32)
      (if (i32.eq (local.get $mode) (i32.const 1)) (then (loop br 0)))
      (if (i32.eq (local.get $mode) (i32.const 2))
        (then (block $done (loop $grow
          (br_if $done (i32.eq (memory.grow (i32.const 1)) (i32.const -1)))
          br $grow))))
      i32.const 42))
  (core instance $guest (instantiate $guest))
  (func (export "run") (param "mode" u32) (result u32)
    (canon lift (core func $guest "run"))))
