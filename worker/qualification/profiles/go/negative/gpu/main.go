// Fixture: GPU access is outside the falcone:host capability host.
package main

import "github.com/NVIDIA/go-nvml/pkg/nvml"

func main() { _ = nvml.Init }
