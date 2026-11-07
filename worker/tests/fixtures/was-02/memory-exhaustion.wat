;; Grows linear memory 1 MiB at a time with no declared maximum, so only the store's
;; guest-memory limit stops it. A denied growth traps; the host reports the limit outcome.
(component
  (core module $guest
    (memory 1)
    (func (export "run")
      (loop $again
        (if (i32.eq (memory.grow (i32.const 16)) (i32.const -1))
          (then unreachable))
        br $again)))
  (core instance $guest (instantiate $guest))
  (func (export "run") (canon lift (core func $guest "run"))))
