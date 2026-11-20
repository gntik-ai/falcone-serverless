package main

import (
	"crypto/sha256"
	"encoding/hex"
	"falcone-profile/bindings"
	"strconv"
)

func init() { bindings.Handle = transform }
func transform(input []byte) ([]byte, error) {
	sum := sha256.Sum256(input)
	return []byte(strconv.Quote(hex.EncodeToString(sum[:]))), nil
}
func main() {}
