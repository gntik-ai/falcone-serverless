;; One large function keeps compiler address-space pressure distinct from input size.
;; Expand OPERATIONS to 131072 multiply/add pairs; never execute this function.
(component (core module
  (func (param i32) (result i32)
    local.get 0
    ;; OPERATIONS
  )
)
  ;; HANDLER
)
