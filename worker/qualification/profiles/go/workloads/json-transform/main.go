// Representative workload: a pure JSON transformation. It imports nothing from the host
// beyond the handler registration, so the component has no falcone:host imports.
package main

import (
	"encoding/json"
	"sort"
	"strings"

	"falcone.invalid/profile/bindings"
)

type item struct {
	Name     string `json:"name"`
	Price    int64  `json:"price"`
	Quantity int64  `json:"quantity"`
}

type request struct {
	Items []item `json:"items"`
}

type response struct {
	Count int      `json:"count"`
	Total int64    `json:"total"`
	Names []string `json:"names"`
}

func init() { bindings.Handle(handle) }

func main() {}

func handle(params []byte) ([]byte, *bindings.HandlerError) {
	var in request
	if err := json.Unmarshal(params, &in); err != nil {
		return nil, &bindings.HandlerError{Invalid: true, Message: "invalid JSON input"}
	}
	var total int64
	names := make([]string, 0, len(in.Items))
	for _, it := range in.Items {
		total += it.Price * it.Quantity
		names = append(names, strings.ToUpper(it.Name))
	}
	sort.Strings(names)
	out, err := json.Marshal(response{Count: len(in.Items), Total: total, Names: names})
	if err != nil {
		return nil, &bindings.HandlerError{Message: "handler failed"}
	}
	return out, nil
}
