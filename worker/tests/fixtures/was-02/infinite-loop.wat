;; A guest that never yields: only epoch interruption (or process termination) can end it.
(component
  (core module $guest
    (func (export "run")
      (loop br 0)))
  (core instance $guest (instantiate $guest))
  (func (export "run") (canon lift (core func $guest "run"))))
