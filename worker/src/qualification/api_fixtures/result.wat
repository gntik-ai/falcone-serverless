(component
  ;; IMPORTS
  (type $bytes (list u8))
  (type $error (variant (case "invalid-input" string) (case "failed" string)))
  (type $result (result $bytes (error $error)))
  (type $handle (func (param "params" $bytes) (result $result)))
  (core module $guest
    ;; CORE_IMPORTS
    (memory (export "memory") 1)
    (data (i32.const 1024) "\7b\22\73\74\61\74\75\73\43\6f\64\65\22\3a\32\30\30\2c\22\62\6f\64\79\22\3a\7b\22\63\72\65\64\65\6e\74\69\61\6c\5f\69\6e\5f\6d\65\6d\6f\72\79\22\3a\66\61\6c\73\65\7d\7d")
    (global $heap (mut i32) (i32.const 2048))
    (func (export "realloc") (param i32 i32 i32) (param $size i32) (result i32)
      (local $ptr i32)
      global.get $heap local.tee $ptr local.get $size i32.add global.set $heap
      local.get $ptr)
    (func (export "handle") (param $ptr i32) (param $len i32) (result i32)
      (local $i i32)
      (loop $scan
        (if (i64.eq (i64.load (local.get $i)) (i64.const 7146496162766480755))
          (then (i32.store8 (i32.const 1073) (i32.const 116))
                (i32.store8 (i32.const 1074) (i32.const 114))
                (i32.store8 (i32.const 1075) (i32.const 117))
                (i32.store8 (i32.const 1076) (i32.const 101))
                (i32.store8 (i32.const 1077) (i32.const 32))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $scan (i32.lt_u (local.get $i) (i32.const 65529))))
i32.const 4 i32.const 1024 i32.store
      i32.const 8 i32.const 56 i32.store
      i32.const 0)
  )
  (core instance $guest (instantiate $guest ;; CORE_ARGS
  ))
  (func $handle (type $handle) (canon lift (core func $guest "handle")
    (memory $guest "memory") (realloc (func $guest "realloc")) string-encoding=utf8))
  (instance $handler
    (export "handler-error" (type $error))
    (export "handle" (func $handle)))
  (export "falcone:host/handler@0.2.0" (instance $handler))
)
