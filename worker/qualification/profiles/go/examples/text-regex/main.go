package main

import (
	"encoding/json"
	"falcone.profile/bindings"
	"regexp"
)

func transform(input []byte) ([]byte, error) {
	var request struct {
		Text string `json:"text"`
	}
	if err := json.Unmarshal(input, &request); err != nil {
		return nil, err
	}
	matches := regexp.MustCompile(`order-([0-9]+)`).FindAllStringSubmatch(request.Text, -1)
	orders := make([]string, 0, len(matches))
	for _, match := range matches {
		orders = append(orders, match[1])
	}
	return json.Marshal(struct {
		Orders []string `json:"orders"`
	}{orders})
}

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, length uint32) uint32 { return bindings.Invoke(ptr, length, transform) }
func main()                            {}
