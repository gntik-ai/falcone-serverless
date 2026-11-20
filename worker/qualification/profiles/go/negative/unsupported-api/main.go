package main

import (
	"reflect"
)

func main() {
	_ = reflect.MakeFunc(reflect.TypeOf(func() {}), nil)
}
