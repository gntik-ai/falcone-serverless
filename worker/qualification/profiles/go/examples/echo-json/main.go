package main

import "falcone-profile/bindings"

func init() { bindings.Handle = transform }
func transform(input []byte) ([]byte, error) {
	return append([]byte(nil), input...), nil
}
func main() {}
