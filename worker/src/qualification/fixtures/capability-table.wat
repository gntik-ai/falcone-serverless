(component
  (core module $guest
    (table 1 funcref)
    (func (export "grow") (param $elements i32) (result i32)
      (table.grow (ref.null func) (local.get $elements))))
  (core instance $guest (instantiate $guest))
  (func (export "grow") (param "elements" u32) (result s32)
    (canon lift (core func $guest "grow"))))
