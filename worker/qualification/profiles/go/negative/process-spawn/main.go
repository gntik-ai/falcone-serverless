// Fixture: spawning a process is not available in Wasm mode.
package main

import "os/exec"

func main() { _ = exec.Command("true") }
