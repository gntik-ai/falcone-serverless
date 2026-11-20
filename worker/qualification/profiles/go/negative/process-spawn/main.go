package main

import "os/exec"

func main() { _ = exec.Command("forbidden") }
