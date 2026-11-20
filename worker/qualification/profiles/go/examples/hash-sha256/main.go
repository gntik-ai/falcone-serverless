package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"profile.invalid/go/bindings"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, hash) }

func hash(input []byte) ([]byte, bool) {
	var params struct {
		Text string `json:"text"`
	}
	if json.Unmarshal(input, &params) != nil {
		return nil, false
	}
	sum := sha256.Sum256([]byte(params.Text))
	output, err := json.Marshal(struct {
		SHA256 string `json:"sha256"`
	}{hex.EncodeToString(sum[:])})
	return output, err == nil
}
func main() {}
