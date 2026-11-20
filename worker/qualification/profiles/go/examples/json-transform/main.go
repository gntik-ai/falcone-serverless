package main

import (
	"encoding/json"
	"profile.invalid/go/bindings"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, transform) }

func transform(input []byte) ([]byte, bool) {
	var params struct {
		Values []int `json:"values"`
	}
	if json.Unmarshal(input, &params) != nil {
		return nil, false
	}
	total := 0
	for _, value := range params.Values {
		total += value
	}
	output, err := json.Marshal(struct {
		Count int `json:"count"`
		Total int `json:"total"`
	}{len(params.Values), total})
	return output, err == nil
}
func main() {}
