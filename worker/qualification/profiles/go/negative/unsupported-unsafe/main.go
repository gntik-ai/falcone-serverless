// Fixture: unsafe memory access is outside the profile.
package main

import "unsafe"

func main() { _ = unsafe.Sizeof(0) }
