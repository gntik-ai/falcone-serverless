// Package bindings is trusted canonical-ABI glue for the qualification fixtures.
// TinyGo owns the sole cabi_realloc export. Input pointers come from that allocator;
// no second arena or Region ownership check rejects runtime-allocated input.
package bindings

import "unsafe"

var output []byte
var result [4]uint32

// Handle accepts memory allocated by TinyGo's runtime cabi_realloc. The result and
// output stay rooted until the invocation's instance is dropped. No allocator export
// or post-return free is added by this package.
func Handle(ptr, size uint32, apply func([]byte) []byte) uint32 {
	input := unsafe.Slice((*byte)(unsafe.Pointer(uintptr(ptr))), int(size))
	output = apply(input)
	result = [4]uint32{0, uint32(uintptr(unsafe.Pointer(unsafe.SliceData(output)))), uint32(len(output)), 0}
	return uint32(uintptr(unsafe.Pointer(&result)))
}
