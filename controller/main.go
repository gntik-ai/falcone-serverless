// Command controller is the falcone-serverless namespaced pool controller.
//
// Placeholder until DEP-03 (#23): it manages no Kubernetes objects yet. The real
// controller writes only standard Deployments, Services and Leases in preprovisioned
// namespaces and is the sole writer of pool replica counts.
package main

import "fmt"

// Version is the controller's build identity.
const Version = "0.1.0"

func main() {
	fmt.Printf("falcone-serverless-controller %s\n", Version)
}
