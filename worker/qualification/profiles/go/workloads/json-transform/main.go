package main

import (
	"encoding/json"
	"falcone.local/go-profile/bindings"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, transform) }

func transform(input []byte) []byte {
	var params struct {
		Name  string `json:"name"`
		Count int    `json:"count"`
	}
	if json.Unmarshal(input, &params) != nil {
		return []byte(`null`)
	}
	value := struct {
		Count int    `json:"count"`
		Name  string `json:"name"`
	}{params.Count + 1, params.Name}
	output, _ := json.Marshal(value)
	return output
}
func main() {}
