package bindings

//go:wasmimport falcone:host/events@0.2.0 publish
func publish(destination *byte, destinationLen uint32, payload *byte, payloadLen uint32, ret *area)

// Publish sends payload to a logical destination. The host resolves the destination within
// the invocation's allowed bindings; routing and authorization never come from the guest.
func Publish(destination string, payload []byte) error {
	var ret area
	dp, dn := stringData(destination)
	pp, pn := bytesData(payload)
	publish(dp, dn, pp, pn, &ret)
	return resultUnit(&ret)
}
