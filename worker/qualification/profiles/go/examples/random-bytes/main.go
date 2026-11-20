// Representative workload: crypto/rand. The Go standard library reads the host's narrow
// wasi:random/random@0.2.0, backed by the operating system CSPRNG. The output never contains
// random data: only whether two draws differ and are not constant.
package main

import (
	"crypto/rand"
	"strconv"

	"falcone.invalid/profile/bindings"
)

func main() {}

func init() {
	bindings.Handle(func(params []byte) ([]byte, *bindings.Error) {
		first := make([]byte, 32)
		second := make([]byte, 32)
		if _, err := rand.Read(first); err != nil {
			return nil, bindings.Failed("entropy unavailable")
		}
		if _, err := rand.Read(second); err != nil {
			return nil, bindings.Failed("entropy unavailable")
		}
		differ, varied := false, false
		for i := range first {
			differ = differ || first[i] != second[i]
			varied = varied || first[i] != first[0]
		}
		return []byte(`{"length":` + strconv.Itoa(len(first)) +
			`,"differ":` + strconv.FormatBool(differ) +
			`,"varied":` + strconv.FormatBool(varied) + `}`), nil
	})
}
