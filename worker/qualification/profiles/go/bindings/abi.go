// Package bindings is trusted fixture glue, built only in the offline compiler image.
// TinyGo supplies the sole cabi_realloc export. No separate arena or allocator export.
package bindings

import "unsafe"

const MaxBytes = 1048576

// Region accepts memory from TinyGo's canonical allocator. The canonical ABI has
// already checked the memory range. Wasm bounds checks still apply to every read.
func Region(ptr, length uint32) []byte {
	if length > MaxBytes || uint64(ptr)+uint64(length) > 1<<32 {
		panic("canonical input exceeds fixture bound")
	}
	if length == 0 {
		return nil
	}
	return unsafe.Slice((*byte)(unsafe.Pointer(uintptr(ptr))), int(length))
}

// Roots keep the output and canonical result area alive until instance teardown.
// A fresh instance handles exactly one invocation; no state is reused across tenants.
var output []byte
var result [4]uint32

func Invoke(ptr, length uint32, handle func([]byte) ([]byte, error)) uint32 {
	var err error
	output, err = handle(Region(ptr, length))
	result = [4]uint32{}
	if err != nil {
		// Fixed message: fixture parse errors never echo input or tenant data.
		output = []byte("invalid input")
		result[0] = 1 // result.err
		result[1] = 0 // handler-error.invalid-input
		result[2] = uint32(uintptr(unsafe.Pointer(unsafe.SliceData(output))))
		result[3] = uint32(len(output))
	} else {
		if len(output) > MaxBytes {
			panic("canonical output exceeds fixture bound")
		}
		result[1] = uint32(uintptr(unsafe.Pointer(unsafe.SliceData(output))))
		result[2] = uint32(len(output))
	}
	return uint32(uintptr(unsafe.Pointer(&result[0])))
}
