package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"falcone.profile/bindings"
)

func transform(input []byte) ([]byte, error) {
	var request struct {
		Message string `json:"message"`
	}
	if err := json.Unmarshal(input, &request); err != nil {
		return nil, err
	}
	digest := sha256.Sum256([]byte(request.Message))
	return json.Marshal(struct {
		SHA256 string `json:"sha256"`
	}{hex.EncodeToString(digest[:])})
}

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, length uint32) uint32 { return bindings.Invoke(ptr, length, transform) }
func main()                            {}
