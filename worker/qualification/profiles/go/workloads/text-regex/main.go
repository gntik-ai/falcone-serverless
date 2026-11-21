package main

import (
	"encoding/json"
	"falcone.local/go-profile/bindings"
	"regexp"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, transform) }

func transform(input []byte) []byte {
	var params struct {
		Text string `json:"text"`
	}
	if json.Unmarshal(input, &params) != nil {
		return []byte(`null`)
	}
	output, _ := json.Marshal(regexp.MustCompile(`[0-9]+`).FindAllString(params.Text, -1))
	return output
}
func main() {}
