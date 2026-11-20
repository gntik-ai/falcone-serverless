package bindings

//go:wasmimport falcone:host/invocation-context@0.2.0 invocation-key
func invocationKey(ret *area)

//go:wasmimport falcone:host/invocation-context@0.2.0 attempt-id
func attemptID(ret *area)

//go:wasmimport falcone:host/invocation-context@0.2.0 idempotency-key
func idempotencyKey(ret *area)

func contextString(call func(*area)) string {
	var ret area
	call(&ret)
	return liftString(ret[0], ret[1])
}

// InvocationKey, AttemptID and IdempotencyKey are read-only identifiers from the verified
// invocation context. They are not credentials.
func InvocationKey() string  { return contextString(invocationKey) }
func AttemptID() string      { return contextString(attemptID) }
func IdempotencyKey() string { return contextString(idempotencyKey) }
