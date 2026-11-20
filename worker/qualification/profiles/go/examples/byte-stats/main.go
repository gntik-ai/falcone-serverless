// Representative workload: byte, word and line counts of the request bytes, with strings,
// strconv and sort only. It imports nothing from the host.
package main

import (
	"sort"
	"strconv"
	"strings"

	"falcone.invalid/profile/bindings"
)

func main() {}

func init() {
	bindings.Handle(func(params []byte) ([]byte, *bindings.Error) {
		text := string(params)
		words := strings.Fields(text)
		counts := map[string]int{}
		for _, word := range words {
			counts[strings.ToLower(word)]++
		}
		keys := make([]string, 0, len(counts))
		for key := range counts {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		top := ""
		for _, key := range keys {
			if top == "" || counts[key] > counts[top] {
				top = key
			}
		}
		return []byte(`{"bytes":` + strconv.Itoa(len(params)) +
			`,"words":` + strconv.Itoa(len(words)) +
			`,"lines":` + strconv.Itoa(strings.Count(text, "\n")+1) +
			`,"top":` + strconv.Quote(top) + `}`), nil
	})
}
