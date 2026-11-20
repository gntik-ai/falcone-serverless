// Fixture: go:linkname reaches runtime symbols that the profile does not expose.
package main

import _ "unsafe"

//go:linkname nanotime runtime.nanotime
func nanotime() int64

func main() { _ = nanotime() }
