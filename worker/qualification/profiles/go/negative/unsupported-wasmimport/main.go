// Fixture: a guest may not declare its own imports; only the trusted bindings do.
package main

//go:wasmimport wasi_snapshot_preview1 fd_write
func fdWrite(fd, iovs, count, written uint32) uint32

func main() { _ = fdWrite }
