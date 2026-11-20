package main

import (
	"encoding/json"
	"falcone.profile/bindings"
	"sort"
	"strings"
)

func transform(input []byte) ([]byte, error) {
	var request struct {
		Items []struct {
			Name     string `json:"name"`
			Price    int    `json:"price"`
			Quantity int    `json:"quantity"`
		} `json:"items"`
	}
	if err := json.Unmarshal(input, &request); err != nil {
		return nil, err
	}
	names := make([]string, 0, len(request.Items))
	total := 0
	for _, item := range request.Items {
		names = append(names, strings.ToUpper(item.Name))
		total += item.Price * item.Quantity
	}
	sort.Strings(names)
	return json.Marshal(struct {
		Names []string `json:"names"`
		Total int      `json:"total"`
	}{names, total})
}

//go:wasmexport falcone:host/handler@0.2.0#handle
func handle(ptr, length uint32) uint32 { return bindings.Invoke(ptr, length, transform) }
func main()                            {}
