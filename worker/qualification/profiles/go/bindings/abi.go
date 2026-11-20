// Package bindings is the trusted, hand-written canonical-ABI layer between a Go guest and the
// falcone:host@0.2.0 capability host. It is reviewed with the profile and is never tenant code.
//
// Memory: the TinyGo wasip2 runtime already exports the single core `cabi_realloc`, and the
// host calls it to place every list or string it hands to the guest. These bindings therefore
// declare no `cabi_realloc` of their own and never allocate through a private arena: lifting
// reads whatever buffer the runtime allocator returned. The profile builds with `-gc=leaking`,
// so nothing the host wrote into is ever reclaimed or moved during one invocation.
package bindings

import "unsafe"

// HostError is the failure vocabulary of falcone:host/cancellation. It never carries
// credentials or binding details.
type HostError uint8

const (
	Denied HostError = iota
	LimitExceeded
	Timeout
	Cancelled
)

func (e HostError) Error() string {
	switch e {
	case Denied:
		return "denied"
	case LimitExceeded:
		return "limit-exceeded"
	case Timeout:
		return "timeout"
	case Cancelled:
		return "cancelled"
	}
	return "host error"
}

// area is a canonical-ABI return area: four aligned words, enough for every result in the world.
type area [4]uint32

// tag is the one-byte discriminant at offset 0. The host writes only that byte, so the rest of
// word 0 is masked off.
func (a *area) tag() uint32 { return a[0] & 0xff }

// unitError is the one-byte error case at offset 1 of result<_, host-error>.
func (a *area) unitError() HostError { return HostError((a[0] >> 8) & 0xff) }

// payloadError is the one-byte error case at offset 4, where a four-byte aligned payload starts.
func (a *area) payloadError() HostError { return HostError(a[1] & 0xff) }

func address[T any](p *T) uint32 { return uint32(uintptr(unsafe.Pointer(p))) }

// stringData returns the pointer and length of s for lowering. An empty string lowers as 0, 0.
func stringData(s string) (*byte, uint32) {
	if len(s) == 0 {
		return nil, 0
	}
	return unsafe.StringData(s), uint32(len(s))
}

func bytesData(b []byte) (*byte, uint32) {
	if len(b) == 0 {
		return nil, 0
	}
	return unsafe.SliceData(b), uint32(len(b))
}

// lift copies the host-written list at ptr, len into guest-owned memory.
func lift(ptr, length uint32) []byte {
	if length == 0 {
		return []byte{}
	}
	view := unsafe.Slice((*byte)(unsafe.Pointer(uintptr(ptr))), int(length))
	return append([]byte(nil), view...)
}

func liftString(ptr, length uint32) string { return string(lift(ptr, length)) }

// resultUnit decodes result<_, host-error>: tag at 0, the error case at 1.
func resultUnit(a *area) error {
	if a.tag() == 0 {
		return nil
	}
	return a.unitError()
}

// resultHandle decodes result<own<resource>, host-error>: tag at 0, the handle or case at 4.
func resultHandle(a *area) (uint32, error) {
	if a.tag() == 0 {
		return a[1], nil
	}
	return 0, a.payloadError()
}

// resultList decodes result<list<u8>, host-error>: tag at 0, the list at 4 and 8.
func resultList(a *area) ([]byte, error) {
	if a.tag() == 0 {
		return lift(a[1], a[2]), nil
	}
	return nil, a.payloadError()
}
