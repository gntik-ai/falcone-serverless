// Fixture: cgo loads native code, which never runs in Wasm mode.
package main

/*
#include <stdio.h>
*/
import "C"

func main() { C.puts(nil) }
