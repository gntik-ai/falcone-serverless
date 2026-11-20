package bindings

import "unsafe"

//go:wasmimport falcone:host/events@0.2.0 publish
func eventsPublish(destination uint32, destinationLength uint32, payload uint32, payloadLength uint32, result uint32)

// Publish emits an event to a destination the invocation grants. The host answers with
// result<_, host-error>: a tag byte, then the host-error case.
func Publish(destination string, payload []byte) *Error {
	var area [2]byte
	var destinationPointer, payloadPointer uint32
	if len(destination) > 0 {
		destinationPointer = pointer(unsafe.Pointer(unsafe.StringData(destination)))
	}
	if len(payload) > 0 {
		payloadPointer = pointer(unsafe.Pointer(&payload[0]))
	}
	eventsPublish(destinationPointer, uint32(len(destination)), payloadPointer, uint32(len(payload)), pointer(unsafe.Pointer(&area[0])))
	if area[0] != 0 {
		return Failed("publish refused")
	}
	return nil
}
