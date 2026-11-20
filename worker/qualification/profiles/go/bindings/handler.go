package bindings

// HandlerError is falcone:host/handler handler-error.
type HandlerError struct {
	// Invalid selects invalid-input; otherwise the error is failed.
	Invalid bool
	Message string
}

// Func is a guest handler: params and the result are UTF-8 JSON envelopes.
type Func func(params []byte) ([]byte, *HandlerError)

const unregistered = "handler is not registered"

var (
	registered Func
	// keep holds the result buffer until the host has copied it out.
	keep  []byte
	reply area
)

// Handle registers the guest's handler from an init function.
func Handle(f Func) { registered = f }

// handle is the one export of the component. wit-component names the core export of
// `handler.handle` exactly this way. result<list<u8>, handler-error> is returned through a
// return area: tag at 0, then either the list at 4, 8 or the error case at 4 and its string
// at 8, 12.
//
//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(params, length uint32) uint32 {
	input := lift(params, length)
	if registered == nil {
		return fail(false, unregistered)
	}
	output, err := registered(input)
	if err != nil {
		return fail(err.Invalid, err.Message)
	}
	keep = output
	p, n := bytesData(keep)
	reply = area{0, address(p), n, 0}
	return address(&reply)
}

func fail(invalid bool, message string) uint32 {
	var kind uint32 = 1
	if invalid {
		kind = 0
	}
	p, n := stringData(message)
	reply = area{1, kind, address(p), n}
	return address(&reply)
}
