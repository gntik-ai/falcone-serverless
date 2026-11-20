// Package bindings is trusted fixture glue for falcone:host/handler@0.2.0.
// TinyGo's wasip2 runtime supplies the sole canonical allocator. Never add an
// allocator export here: host-lowered inputs are runtime allocations, not entries
// in a second arena. Buffers stay rooted until canonical post-return.
package bindings

import "unsafe"

var Handle func([]byte) ([]byte, error)

var inputRoot []byte
var outputRoot []byte
var result [4]uint32

//export falcone:host/handler@0.2.0#handle
func handle(pointer, length uint32) uint32 {
	if length > 1048576 {
		panic("handler input limit")
	}
	// Accept memory from TinyGo's cabi allocator directly. Wasm memory bounds
	// remain enforced; there is deliberately no arena-ownership predicate.
	inputRoot = unsafe.Slice((*byte)(unsafe.Pointer(uintptr(pointer))), int(length))
	output, err := Handle(inputRoot)
	result = [4]uint32{}
	if err != nil {
		// Fixed diagnostic only; never expose input or a library error string.
		outputRoot = []byte("invalid input")
		result[0] = 1 // result.err
		result[1] = 0 // handler-error.invalid-input
		result[2] = address(outputRoot)
		result[3] = uint32(len(outputRoot))
	} else {
		if len(output) > 1048576 {
			panic("handler output limit")
		}
		outputRoot = output
		result[1] = address(outputRoot)
		result[2] = uint32(len(outputRoot))
	}
	return uint32(uintptr(unsafe.Pointer(&result[0])))
}

func address(bytes []byte) uint32 {
	if len(bytes) == 0 {
		return 0
	}
	return uint32(uintptr(unsafe.Pointer(&bytes[0])))
}

//export cabi_post_falcone:host/handler@0.2.0#handle
func postReturn(_ uint32) {
	inputRoot = nil
	outputRoot = nil
	result = [4]uint32{}
}
