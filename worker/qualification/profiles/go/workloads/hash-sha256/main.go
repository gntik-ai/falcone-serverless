// Representative workload: SHA-256 with the standard library. It imports nothing from the host.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"

	"falcone.invalid/profile/bindings"
)

type request struct {
	Message string `json:"message"`
}

type response struct {
	SHA256 string `json:"sha256"`
}

func init() { bindings.Handle(handle) }

func main() {}

func handle(params []byte) ([]byte, *bindings.HandlerError) {
	var in request
	if err := json.Unmarshal(params, &in); err != nil {
		return nil, &bindings.HandlerError{Invalid: true, Message: "invalid JSON input"}
	}
	sum := sha256.Sum256([]byte(in.Message))
	out, err := json.Marshal(response{SHA256: hex.EncodeToString(sum[:])})
	if err != nil {
		return nil, &bindings.HandlerError{Message: "handler failed"}
	}
	return out, nil
}
