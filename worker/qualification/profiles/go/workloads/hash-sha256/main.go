package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"falcone.local/go-profile/bindings"
)

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, size uint32) uint32 { return bindings.Handle(ptr, size, transform) }

func transform(input []byte) []byte {
	var params string
	if json.Unmarshal(input, &params) != nil {
		return []byte(`null`)
	}
	digest := sha256.Sum256([]byte(params))
	output, _ := json.Marshal(struct {
		SHA256 string `json:"sha256"`
	}{hex.EncodeToString(digest[:])})
	return output
}
func main() {}
