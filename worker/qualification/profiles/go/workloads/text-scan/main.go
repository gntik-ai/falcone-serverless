// Representative workload: scanning text for order numbers and an ISO date with strings only
// (no regexp, which TinyGo supports only partially). It imports nothing from the host.
package main

import (
	"encoding/json"
	"strings"

	"falcone.invalid/profile/bindings"
)

type request struct {
	Text string `json:"text"`
}

type date struct {
	Year  string `json:"year"`
	Month string `json:"month"`
	Day   string `json:"day"`
}

type response struct {
	Orders []string `json:"orders"`
	Date   *date    `json:"date"`
}

func init() { bindings.Handle(handle) }

func main() {}

// digits is the length of the run of ASCII digits at the start of s.
func digits(s string) int {
	n := 0
	for n < len(s) && s[n] >= '0' && s[n] <= '9' {
		n++
	}
	return n
}

func orders(text string) []string {
	out := []string{}
	const key = "order "
	for {
		at := strings.Index(text, key)
		if at < 0 {
			return out
		}
		text = text[at+len(key):]
		n := digits(text)
		if n > 0 {
			out = append(out, text[:n])
			text = text[n:]
		}
	}
}

// firstDate finds the first yyyy-mm-dd.
func firstDate(text string) *date {
	for at := 0; at+10 <= len(text); at++ {
		w := text[at : at+10]
		if digits(w) >= 4 && w[4] == '-' && digits(w[5:]) >= 2 && w[7] == '-' && digits(w[8:]) == 2 {
			return &date{Year: w[0:4], Month: w[5:7], Day: w[8:10]}
		}
	}
	return nil
}

func handle(params []byte) ([]byte, *bindings.HandlerError) {
	var in request
	if err := json.Unmarshal(params, &in); err != nil {
		return nil, &bindings.HandlerError{Invalid: true, Message: "invalid JSON input"}
	}
	out, err := json.Marshal(response{Orders: orders(in.Text), Date: firstDate(in.Text)})
	if err != nil {
		return nil, &bindings.HandlerError{Message: "handler failed"}
	}
	return out, nil
}
