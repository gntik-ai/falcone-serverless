package main

import (
	"os/exec"
)

func main() {
	_ = exec.Command("sh", "-c", "id").Run()
}
