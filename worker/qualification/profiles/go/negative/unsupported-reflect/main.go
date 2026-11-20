// Fixture: TinyGo supports only part of reflect, so the profile does not accept it directly.
package main

import "reflect"

func main() { _ = reflect.TypeOf(0) }
