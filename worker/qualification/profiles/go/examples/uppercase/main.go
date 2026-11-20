package main

import (
	"falcone-profile/bindings"
	"strconv"
	"strings"
)

func init() { bindings.Handle = transform }
func transform(input []byte) ([]byte, error) {
	text, err := strconv.Unquote(string(input))
	if err != nil {
		return nil, err
	}
	return []byte(strconv.Quote(strings.ToUpper(text))), nil
}
func main() {}
