// Package bindings is the trusted, hand-written canonical-ABI layer for the falcone:host
// function world. It has no dependency outside the Go standard library and it defines no
// cabi_realloc: the pinned TinyGo 0.44.0 wasip2 runtime already exports exactly one, and every
// list or string the host lowers into the guest is allocated by it. The trusted image copies
// this directory into the build; a tenant tree may never contain files under bindings/.
package bindings

import "unsafe"

// Error is the handler-error variant: Kind 0 is invalid-input, Kind 1 is failed.
type Error struct {
	Kind    uint8
	Message string
}

// Invalid and Failed build the two handler-error cases.
func Invalid(message string) *Error { return &Error{Kind: 0, Message: message} }
func Failed(message string) *Error  { return &Error{Kind: 1, Message: message} }

var (
	handler func(params []byte) ([]byte, *Error)
	// returnArea is the canonical-ABI return area of result<list<u8>, handler-error>:
	// tag u8 at 0; ok list (ptr, len) at 4 and 8; err variant tag at 4, string (ptr, len) at 8
	// and 12. It is a global, so the host reads it after this call returns.
	returnArea [16]byte
	// live keeps the lowered result reachable until the host has copied it out.
	live [][]byte
)

// Handle registers the function's handler. It is called once, before the first invocation.
func Handle(f func(params []byte) ([]byte, *Error)) { handler = f }

func pointer(p unsafe.Pointer) uint32 { return uint32(uintptr(p)) }

func word(offset uintptr, value uint32) {
	*(*uint32)(unsafe.Add(unsafe.Pointer(&returnArea[0]), offset)) = value
}

// raw returns the memory of a lowered list as a Go slice without copying it.
func raw(p unsafe.Pointer, length uint32) []byte {
	if length == 0 {
		return nil
	}
	return unsafe.Slice((*byte)(p), length)
}

// lower stores bytes for the host. An empty value needs no allocation.
func lower(value []byte) (uint32, uint32) {
	if len(value) == 0 {
		return pointer(unsafe.Pointer(&returnArea[0])), 0
	}
	live = [][]byte{value}
	return pointer(unsafe.Pointer(&value[0])), uint32(len(value))
}

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(params unsafe.Pointer, length uint32) unsafe.Pointer {
	// Copy first: params lives in memory the runtime allocator handed to the host.
	input := append([]byte(nil), raw(params, length)...)
	area := unsafe.Pointer(&returnArea[0])
	if handler == nil {
		returnArea[0] = 1
		returnArea[4] = 1
		p, n := lower([]byte("no handler registered"))
		word(8, p)
		word(12, n)
		return area
	}
	output, failure := handler(input)
	if failure != nil {
		returnArea[0] = 1
		returnArea[4] = failure.Kind
		p, n := lower([]byte(failure.Message))
		word(8, p)
		word(12, n)
		return area
	}
	returnArea[0] = 0
	p, n := lower(output)
	word(4, p)
	word(8, n)
	return area
}
