// Fixture: build hooks are never run; nothing is generated or installed.
package main

//go:generate sh -c "echo never run"

func main() {}
