// Project-authored wasm32 canonical ABI for handler.handle in falcone:host@0.2.0.
// This is qualification source, UNVERIFIED until the pinned trusted-image build and
// restricted cluster replay pass. No generated or third-party binding module is used.
package bindings

import "unsafe"

const MaxBytes uint32 = 1048576

// All canonical allocations belong to one synchronous invocation. They remain live
// through post-return; no Go heap pointer is returned to the host. Exhaustion traps.
var arena [8388608]byte
var used uint32
var result [4]uint32

func base() uint32 { return uint32(uintptr(unsafe.Pointer(&arena[0]))) }

func Region(ptr, size uint32) []byte {
	if size == 0 {
		return nil
	}
	start := base()
	if size > MaxBytes || ptr < start || ptr-start > used || size > used-(ptr-start) {
		panic("canonical ABI bounds")
	}
	return arena[ptr-start : ptr-start+size]
}

//go:wasmexport cabi_realloc
func Realloc(old, oldSize, align, size uint32) uint32 {
	if align == 0 || align > 16 || align&(align-1) != 0 || size > MaxBytes {
		panic("canonical ABI allocation")
	}
	previous := Region(old, oldSize)
	if size == 0 {
		return 0
	}
	// Align the absolute pointer, not just the offset of the byte-aligned arena.
	ptr := (base() + used + align - 1) & ^(align - 1)
	offset := ptr - base()
	if offset > uint32(len(arena)) || size > uint32(len(arena))-offset {
		panic("canonical ABI arena exhausted")
	}
	used = offset + size
	copy(arena[offset:used], previous)
	return ptr
}

func Bytes(data []byte) (uint32, uint32) {
	if len(data) > int(MaxBytes) {
		panic("canonical ABI byte limit")
	}
	size := uint32(len(data))
	ptr := Realloc(0, 0, 1, size)
	copy(Region(ptr, size), data)
	return ptr, size
}

// The result layout is 16 bytes, aligned to four: tag at 0; success list
// pointer/length at 4/8; error variant at 4 and its string pointer/length at 8/12.
func Handle(ptr, size uint32, handler func([]byte) ([]byte, bool)) uint32 {
	data, ok := handler(Region(ptr, size))
	result = [4]uint32{}
	if ok {
		result[1], result[2] = Bytes(data)
	} else {
		result[0] = 1
		result[2], result[3] = Bytes([]byte("invalid input"))
	}
	return uint32(uintptr(unsafe.Pointer(&result[0])))
}

//go:wasmexport cabi_post_falcone:host/handler@0.2.0#handle
func PostReturn(_ uint32) {
	// One invocation per instance in the worker. Clearing also makes this safe for
	// a subsequent synchronous call; never reset during a host allocation callback.
	clear(arena[:used])
	used = 0
	result = [4]uint32{}
}
