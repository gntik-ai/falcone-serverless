package main

import (
	"encoding/json"
	"profile.invalid/go/bindings"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, publish) }

func publish(input []byte) ([]byte, bool) {
	// A synthetic fixed binding, never a tenant-selected scope or destination.
	data, ok := bindings.Read("tenants/synthetic/rate/ws-synthetic-0001/item")
	if !ok {
		return []byte(`{"published":false}`), true
	}
	if !bindings.Publish("console.openapi.spec.accessed", data) {
		return nil, false
	}
	output, err := json.Marshal(struct {
		Published bool `json:"published"`
	}{true})
	return output, err == nil
}
func main() {}
