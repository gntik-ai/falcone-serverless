// Project-authored canonical ABI imports. Only the storage-events example links
// this file; pure examples have no host imports. All handles are dropped.
package bindings

import "unsafe"

//go:wasmimport falcone:host/storage@0.2.0 open-object
func openObject(key, size, result uint32)

//go:wasmimport falcone:host/storage@0.2.0 read-object
func readObject(handle, result uint32)

//go:wasmimport falcone:host/storage@0.2.0 [resource-drop]object
func dropObject(handle uint32)

//go:wasmimport falcone:host/events@0.2.0 publish
func publish(destination, destinationSize, payload, payloadSize, result uint32)

func Read(key string) ([]byte, bool) {
	ptr, size := Bytes([]byte(key))
	// result<object,host-error>: discriminant byte at 0, payload at 4.
	var opened [2]uint32
	openObject(ptr, size, uint32(uintptr(unsafe.Pointer(&opened[0]))))
	if opened[0] != 0 {
		return nil, false
	}
	handle := opened[1]
	defer dropObject(handle)
	// result<list<u8>,host-error>: discriminant at 0, pointer/length at 4/8.
	var read [3]uint32
	readObject(handle, uint32(uintptr(unsafe.Pointer(&read[0]))))
	if read[0] != 0 {
		return nil, false
	}
	// Keep this borrowed arena region live until post-return; no ambient file IO.
	return Region(read[1], read[2]), true
}

func Publish(destination string, data []byte) bool {
	dest, destSize := Bytes([]byte(destination))
	ptr, size := Bytes(data)
	// result<_,host-error>: two bytes (result tag, error payload).
	var result [2]byte
	publish(dest, destSize, ptr, size, uint32(uintptr(unsafe.Pointer(&result[0]))))
	return result[0] == 0
}
