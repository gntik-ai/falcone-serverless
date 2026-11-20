// Representative workload: SHA-256 of the request bytes with crypto/sha256. It imports
// nothing from the host.
package main

import (
	"crypto/sha256"
	"encoding/hex"

	"falcone.invalid/profile/bindings"
)

func main() {}

func init() {
	bindings.Handle(func(params []byte) ([]byte, *bindings.Error) {
		sum := sha256.Sum256(params)
		return []byte(`{"sha256":"` + hex.EncodeToString(sum[:]) + `"}`), nil
	})
}
