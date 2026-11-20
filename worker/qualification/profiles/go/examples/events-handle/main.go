// Minimal handle() example: it publishes its request bytes as one event to the granted
// destination and answers with the byte count. Imports are falcone:host interfaces only.
package main

import (
	"strconv"

	"falcone.invalid/profile/bindings"
)

const topic = "qualification.go.events"

func main() {}

func init() {
	bindings.Handle(func(params []byte) ([]byte, *bindings.Error) {
		if len(params) == 0 {
			return nil, bindings.Invalid("empty input")
		}
		if failure := bindings.Publish(topic, params); failure != nil {
			// A denied or failed host call is a handler error; no binding detail crosses.
			return nil, bindings.Failed("handler failed")
		}
		return []byte(`{"published":` + strconv.Itoa(len(params)) + `}`), nil
	})
}
