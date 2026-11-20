package main

import (
	//go:generate forbidden
	"encoding/json"
)

func main() { _ = json.Valid(nil) }
