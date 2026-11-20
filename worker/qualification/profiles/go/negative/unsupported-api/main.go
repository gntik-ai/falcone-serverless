//go:linkname forbidden
package main

import "reflect"

func main() { _ = reflect.TypeOf(0) }
