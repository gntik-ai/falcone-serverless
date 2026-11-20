// Fixture: net/http is outside the TinyGo subset and the profile's standard-library list.
package main

import "net/http"

func main() { _ = http.StatusOK }
