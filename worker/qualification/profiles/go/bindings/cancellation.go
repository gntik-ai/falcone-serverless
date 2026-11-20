package bindings

//go:wasmimport falcone:host/cancellation@0.2.0 is-cancelled
func isCancelled() uint32

// IsCancelled reports whether the host has cancelled this invocation.
func IsCancelled() bool { return isCancelled() != 0 }
